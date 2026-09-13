//! The database schema.
//!
//! Applied with `CREATE TABLE IF NOT EXISTS`, so opening an existing database is
//! idempotent. There is no migration framework: the agent ships with its data
//! directory, a schema change is a version bump handled in one place, and
//! pulling in a migration crate for four tables would be more machinery than
//! the problem has.
//!
//! # What is deliberately not stored
//!
//! Generated configuration bodies. Only metadata lives here, because config
//! bodies need file semantics — atomic rename, checksum verification, direct
//! diffing — and a BLOB column would fight all three. See ADR-004.

use rusqlite::Connection;

use proxy_application::ports::PortError;

use super::storage_err;

/// The schema version this build expects.
///
/// Recorded in `PRAGMA user_version` so a future build can tell an old database
/// from a new one rather than discovering the difference through a query error.
///
/// Version 2 added the configuration, subscription, and credential tables.
/// Version 3 stores API tokens as salted hashes rather than plaintext.
/// Version 4 adds web sessions.
pub const SCHEMA_VERSION: i64 = 4;

/// Creates every table this build needs.
///
/// # Errors
///
/// Returns [`PortError::Storage`] when a statement fails, or when the existing
/// database was written by a newer build — reading it would risk misinterpreting
/// columns this build does not know about.
pub fn apply(connection: &mut Connection) -> Result<(), PortError> {
    let existing: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| storage_err(format!("cannot read schema version: {e}")))?;

    if existing > SCHEMA_VERSION {
        return Err(storage_err(format!(
            "database schema version {existing} is newer than this build supports \
             ({SCHEMA_VERSION}); refusing to open it"
        )));
    }

    let transaction = connection
        .transaction()
        .map_err(|e| storage_err(format!("cannot begin schema transaction: {e}")))?;

    transaction
        .execute_batch(DDL)
        .map_err(|e| storage_err(format!("cannot apply schema: {e}")))?;

    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|e| storage_err(format!("cannot record schema version: {e}")))?;

    transaction
        .commit()
        .map_err(|e| storage_err(format!("cannot commit schema: {e}")))?;

    Ok(())
}

/// The tables.
///
/// Two choices worth noting:
///
/// * **Audit records have no update or delete path**, matching the domain type.
///   The table is append-only by construction; the statements to mutate it do
///   not exist.
/// * **Instance state is one row per instance**, holding the status as a label
///   plus the optional active config. The label is the same string the domain
///   round-trips through `MihomoStatus::from_label`, so an unrecognized value is
///   rejected on load rather than silently defaulting.
const DDL: &str = r"
CREATE TABLE IF NOT EXISTS instances (
    id              TEXT PRIMARY KEY NOT NULL,
    name            TEXT NOT NULL,
    status          TEXT NOT NULL,
    active_config   TEXT,
    build_version   TEXT,
    build_flavor    TEXT,
    build_raw       TEXT,
    failure_reason  TEXT,
    failure_at      INTEGER,
    updated_at      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS jobs (
    id          TEXT PRIMARY KEY NOT NULL,
    kind        TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    state_kind  TEXT NOT NULL,
    step        TEXT,
    summary     TEXT,
    reason      TEXT,
    degradation TEXT,
    degradation_reason TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS jobs_recent ON jobs (created_at DESC);

CREATE TABLE IF NOT EXISTS audit_entries (
    id              TEXT PRIMARY KEY NOT NULL,
    action          TEXT NOT NULL,
    actor_kind      TEXT NOT NULL,
    actor_uid       INTEGER,
    actor_name      TEXT,
    actor_id        TEXT,
    target_kind     TEXT NOT NULL,
    target_value    TEXT,
    result_kind     TEXT NOT NULL,
    result_reason   TEXT,
    at              INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS audit_recent ON audit_entries (at DESC);

CREATE TABLE IF NOT EXISTS config_versions (
    id              TEXT PRIMARY KEY NOT NULL,
    instance_id     TEXT NOT NULL,
    sequence        INTEGER NOT NULL,
    source_kind     TEXT NOT NULL,
    source_subscription TEXT,
    source_rollback_from TEXT,
    checksum        TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    activated_at    INTEGER,
    UNIQUE (instance_id, sequence)
);

CREATE INDEX IF NOT EXISTS config_versions_by_instance
    ON config_versions (instance_id, sequence DESC);

-- The active pointer lives here rather than in a file so that switching it is a
-- single transaction. A separate file could be renamed atomically, but then the
-- pointer and the version metadata could disagree after a crash with no way to
-- tell which was newer.
CREATE TABLE IF NOT EXISTS config_active (
    instance_id     TEXT PRIMARY KEY NOT NULL,
    version_id      TEXT NOT NULL,
    checksum        TEXT NOT NULL,
    switched_at     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS config_sequences (
    instance_id     TEXT PRIMARY KEY NOT NULL,
    last_sequence   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS subscriptions (
    id              TEXT PRIMARY KEY NOT NULL,
    name            TEXT NOT NULL,
    source_kind     TEXT NOT NULL,
    source_url      TEXT,
    source_user_agent TEXT,
    converter       TEXT NOT NULL,
    target          TEXT NOT NULL,
    enabled         INTEGER NOT NULL,
    schedule_seconds INTEGER,
    last_update_at  INTEGER,
    last_update_kind TEXT,
    last_update_target TEXT,
    last_update_detail TEXT
);

CREATE TABLE IF NOT EXISTS secrets (
    name            TEXT PRIMARY KEY NOT NULL,
    value           TEXT NOT NULL,
    created_at      INTEGER NOT NULL
);

-- Browser sessions for the web interface.
--
-- Separate from `api_principals` because the two have different lifetimes and
-- different revocation stories. An API token is a long-lived credential for a
-- script or a CLI; a session is a short-lived one for a browser, revoked by
-- logging out. Rotating a token must not sign anyone out, and signing out must
-- not disable a script.
--
-- The identifier is stored hashed for the same reason as a token: this table is
-- what an attacker with a database copy would use to impersonate a logged-in
-- user, and reading it must not be the same thing as holding the sessions.
CREATE TABLE IF NOT EXISTS sessions (
    id_hash         TEXT PRIMARY KEY NOT NULL,
    principal       TEXT NOT NULL,
    role            TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    last_seen_at    INTEGER NOT NULL
);

-- Expired sessions are deleted opportunistically on every read rather than by a
-- background task, so a deployment with no traffic still converges without a
-- scheduler. The index makes that sweep cheap.
CREATE INDEX IF NOT EXISTS sessions_last_seen ON sessions (last_seen_at);

-- API tokens are stored as a salted hash, never in plaintext.
--
-- A token is the *only* credential on the TCP listener: over a unix socket the
-- file permissions are the boundary, but a TCP caller is identified by nothing
-- except the token it presents. Storing it in plaintext would mean that reading
-- the database is equivalent to holding every credential, with no way to notice.
--
-- The salt is per row rather than global, so two identical tokens would not
-- produce identical hashes and a stolen database cannot be compared against
-- another. This is what makes a plain SHA-256 acceptable here: tokens are
-- generated as high-entropy random values, not chosen by a human, so there is no
-- dictionary to attack and no reason to pay for a slow hash on every request.
CREATE TABLE IF NOT EXISTS api_principals (
    id              TEXT PRIMARY KEY NOT NULL,
    role            TEXT NOT NULL,
    token_hash      TEXT NOT NULL,
    token_salt      TEXT NOT NULL,
    created_at      INTEGER NOT NULL
);
";

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Connection {
        Connection::open_in_memory().expect("in-memory database")
    }

    #[test]
    fn applying_twice_is_idempotent() {
        let mut connection = memory();
        apply(&mut connection).expect("first apply");
        apply(&mut connection).expect("second apply must be a no-op");

        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn every_expected_table_exists() {
        let mut connection = memory();
        apply(&mut connection).expect("apply");

        for table in [
            "instances",
            "jobs",
            "audit_entries",
            "config_versions",
            "config_active",
            "config_sequences",
            "subscriptions",
            "secrets",
            "api_principals",
        ] {
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .expect("query");
            assert_eq!(count, 1, "{table} must exist");
        }
    }

    /// An older database must gain the new tables rather than being rejected:
    /// `CREATE TABLE IF NOT EXISTS` makes the upgrade additive, so a v1 install
    /// keeps its instances and jobs.
    #[test]
    fn an_older_database_is_upgraded_in_place() {
        let mut connection = memory();
        // A v1 database: the original tables, at the original version.
        connection
            .execute_batch(
                "CREATE TABLE instances (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL,
                     status TEXT NOT NULL, active_config TEXT, build_version TEXT,
                     build_flavor TEXT, build_raw TEXT, failure_reason TEXT,
                     failure_at INTEGER, updated_at INTEGER NOT NULL);
                 INSERT INTO instances (id, name, status, updated_at)
                     VALUES ('default', 'default', 'Running', 1);",
            )
            .expect("v1 schema");
        connection
            .pragma_update(None, "user_version", 1)
            .expect("set v1");

        apply(&mut connection).expect("upgrade must succeed");

        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);

        // The pre-existing row must survive the upgrade.
        let name: String = connection
            .query_row(
                "SELECT name FROM instances WHERE id = 'default'",
                [],
                |row| row.get(0),
            )
            .expect("the existing row must survive");
        assert_eq!(name, "default");

        // And the new tables must now exist.
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='config_versions'",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(count, 1, "the upgrade must add the new tables");
    }

    /// A database from a future build must be refused rather than misread.
    #[test]
    fn a_newer_schema_is_refused() {
        let mut connection = memory();
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .expect("set version");

        let err = apply(&mut connection).expect_err("a newer schema must be refused");
        assert!(
            err.to_string().contains("newer"),
            "the error should explain why: {err}"
        );
    }
}
