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
pub const SCHEMA_VERSION: i64 = 1;

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

        for table in ["instances", "jobs", "audit_entries"] {
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
