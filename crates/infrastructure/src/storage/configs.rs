//! Configuration version storage.
//!
//! Metadata lives in SQLite; bodies live on the filesystem, per ADR-004. That
//! split is not incidental: config bodies need file semantics — atomic replace,
//! checksum verification, direct diffing — and a BLOB column fights all three.
//!
//! # The active pointer
//!
//! "Which version is active" is the one piece of state whose corruption breaks
//! recovery, so it is stored once, in `config_active`, and every read
//! cross-checks it: the pointer's checksum must match the checksum recorded for
//! that version, and the body on disk must hash to the same value. A mismatch is
//! reported, never silently repaired or ignored, because the caller's next move
//! depends on trusting it — rollback re-drives the previous version, and it
//! cannot do that if it does not know which version that was.
//!
//! # Atomicity
//!
//! Bodies are written to a temporary file **in the same directory** and then
//! renamed over the target. Same-directory rename is atomic on POSIX, and it is
//! the only way the target is ever observed either fully-old or fully-new.
//! Writing in place would expose a partially written config, which the kernel
//! could then load.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};

use proxy_application::ports::PortError;
use proxy_application::ports::config_repository::ConfigRepository;
use proxy_domain::configuration::ConfigBody;
use proxy_domain::configuration::version::{ConfigChecksum, ConfigSource, ConfigVersion};
use proxy_domain::shared::id::{ConfigVersionId, MihomoInstanceId};
use proxy_domain::shared::time::Timestamp;

use crate::storage::{SqlitePool, storage_err};

/// File permissions for a stored configuration body.
///
/// A config carries proxy credentials and the controller secret, so it is not
/// world-readable. `0640` keeps it readable by a group member (an operator, or
/// the agent's own group) without opening it to every local user.
pub const CONFIG_FILE_MODE: u32 = 0o640;

/// File permissions for a directory the agent owns.
pub const DIRECTORY_MODE: u32 = 0o750;

/// Stores configuration versions on the filesystem with metadata in SQLite.
#[derive(Debug, Clone)]
pub struct FileConfigRepository {
    pool: SqlitePool,
    configs_dir: PathBuf,
}

impl FileConfigRepository {
    /// Creates a repository, creating the directory if it is absent.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Storage`] when the configs directory cannot be
    /// created with the expected permissions.
    pub async fn new(pool: SqlitePool, configs_dir: impl Into<PathBuf>) -> Result<Self, PortError> {
        let configs_dir = configs_dir.into();
        tokio::fs::create_dir_all(&configs_dir)
            .await
            .map_err(|e| storage_err(format!("cannot create {}: {e}", configs_dir.display())))?;
        set_mode(&configs_dir, DIRECTORY_MODE).await?;
        Ok(Self { pool, configs_dir })
    }

    /// Creates a repository over a directory that already exists.
    ///
    /// Exists because [`AdapterFactory::configs`] is synchronous while creating
    /// a directory is not: the composition root prepares the directories (it is
    /// async) and this constructor then does no I/O. The alternative — blocking
    /// inside a sync factory method — would stall a runtime thread on a
    /// filesystem call.
    ///
    /// [`AdapterFactory::configs`]: proxy_bootstrap::AdapterFactory::configs
    #[must_use]
    pub fn over_existing_dir(pool: SqlitePool, configs_dir: impl Into<PathBuf>) -> Self {
        Self {
            pool,
            configs_dir: configs_dir.into(),
        }
    }

    /// The permissions a configs directory is expected to carry.
    ///
    /// Exposed so the composition root can create the directory with exactly the
    /// mode this adapter would have applied.
    #[must_use]
    pub const fn directory_mode() -> u32 {
        DIRECTORY_MODE
    }

    /// The directory holding configuration bodies.
    #[must_use]
    pub fn configs_dir(&self) -> &Path {
        &self.configs_dir
    }

    /// The path a version's body lives at.
    ///
    /// Derived from the version's own label rather than from its identifier, so
    /// the filename stays stable and human-readable (`v003.yaml`) even if an
    /// identifier format changes. The label is validated as a bare `vNNN` before
    /// use, so a crafted identifier cannot escape the directory.
    fn body_path(&self, version: &ConfigVersion) -> Result<PathBuf, PortError> {
        let label = version.label();
        if !is_safe_label(&label) {
            return Err(storage_err(format!(
                "refusing to build a path from an unsafe version label: {label}"
            )));
        }
        Ok(self.configs_dir.join(format!("{label}.yaml")))
    }

    /// Writes a body atomically with `mode`.
    async fn write_body_atomically(
        &self,
        path: &Path,
        body: &ConfigBody,
        mode: u32,
    ) -> Result<(), PortError> {
        let directory = path
            .parent()
            .ok_or_else(|| storage_err("config body path has no parent"))?
            .to_path_buf();
        let target = path.to_path_buf();
        let contents = body.as_str().to_owned();

        tokio::task::spawn_blocking(move || -> Result<(), PortError> {
            use std::io::Write;

            let mut temp = tempfile::Builder::new()
                .prefix(".tmp-")
                .suffix(".yaml")
                .tempfile_in(&directory)
                .map_err(|e| storage_err(format!("cannot create a temporary file: {e}")))?;

            temp.write_all(contents.as_bytes())
                .map_err(|e| storage_err(format!("cannot write config body: {e}")))?;
            // Flush to the filesystem before the rename, so a crash cannot leave
            // the target name pointing at unwritten data.
            temp.as_file()
                .sync_all()
                .map_err(|e| storage_err(format!("cannot flush config body: {e}")))?;

            set_mode_blocking(temp.path(), mode)?;

            temp.persist(&target)
                .map_err(|e| storage_err(format!("cannot replace {}: {e}", target.display())))?;
            Ok(())
        })
        .await
        .map_err(|e| storage_err(format!("config write task failed: {e}")))?
    }
}

#[async_trait]
impl ConfigRepository for FileConfigRepository {
    async fn list(
        &self,
        instance: &MihomoInstanceId,
        limit: usize,
    ) -> Result<Vec<ConfigVersion>, PortError> {
        let key = instance.as_str().to_owned();
        let rows = self
            .pool
            .with_connection(move |conn| {
                let mut statement = conn
                    .prepare(
                        "SELECT id, instance_id, sequence, source_kind,
                                source_subscription, source_rollback_from,
                                checksum, created_at, activated_at
                         FROM config_versions WHERE instance_id = ?1
                         ORDER BY sequence DESC LIMIT ?2",
                    )
                    .map_err(|e| storage_err(format!("cannot prepare version list: {e}")))?;

                let mapped = statement
                    .query_map(params![key, limit as i64], map_version_row)
                    .map_err(|e| storage_err(format!("cannot list versions: {e}")))?;

                let mut stored = Vec::new();
                for entry in mapped {
                    stored.push(entry.map_err(|e| storage_err(format!("cannot read row: {e}")))?);
                }
                Ok(stored)
            })
            .await?;

        rows.into_iter().map(StoredVersion::into_domain).collect()
    }

    async fn get(&self, id: &ConfigVersionId) -> Result<Option<ConfigVersion>, PortError> {
        let key = id.as_str().to_owned();
        let row = self
            .pool
            .with_connection(move |conn| {
                conn.query_row(
                    "SELECT id, instance_id, sequence, source_kind,
                            source_subscription, source_rollback_from,
                            checksum, created_at, activated_at
                     FROM config_versions WHERE id = ?1",
                    [key.as_str()],
                    map_version_row,
                )
                .optional()
                .map_err(|e| storage_err(format!("cannot read version: {e}")))
            })
            .await?;

        row.map(StoredVersion::into_domain).transpose()
    }

    async fn next_sequence(&self, instance: &MihomoInstanceId) -> Result<u64, PortError> {
        let key = instance.as_str().to_owned();
        let sequence: i64 = self
            .pool
            .with_connection(move |conn| {
                // One statement, so the increment is atomic even under
                // concurrent activations: SQLite evaluates the upsert and the
                // returning clause in a single transaction.
                conn.query_row(
                    "INSERT INTO config_sequences (instance_id, last_sequence)
                     VALUES (?1, 1)
                     ON CONFLICT(instance_id) DO UPDATE SET last_sequence = last_sequence + 1
                     RETURNING last_sequence",
                    [key.as_str()],
                    |row| row.get(0),
                )
                .map_err(|e| storage_err(format!("cannot allocate a sequence number: {e}")))
            })
            .await?;

        u64::try_from(sequence)
            .map_err(|_| storage_err(format!("sequence number went negative: {sequence}")))
    }

    async fn save(&self, version: &ConfigVersion, body: &ConfigBody) -> Result<(), PortError> {
        // The checksum is recomputed from the bytes being written rather than
        // trusted from the caller: storing a version whose recorded checksum does
        // not match its own body would make every later verification fail.
        let actual = body.checksum();
        if &actual != version.checksum() {
            return Err(storage_err(format!(
                "refusing to store version {} with a mismatched checksum \
                 (recorded {}, actual {})",
                version.id().as_str(),
                version.checksum().as_str(),
                actual.as_str()
            )));
        }

        let path = self.body_path(version)?;
        let stored = StoredVersion::from_domain(version);

        // Decide whether this version may be stored *before* touching the body.
        //
        // Writing first and cleaning up on rejection looks equivalent but is not:
        // a rejected rewrite targets the same path as the version it collides
        // with, so "remove the body I just wrote" would delete the body of the
        // version that is already stored and must not change. Checking first
        // makes the failure a true no-op.
        let record = stored.clone();
        self.pool
            .with_connection(move |conn| {
                let existing: Option<(String, String)> = conn
                    .query_row(
                        "SELECT checksum, instance_id FROM config_versions WHERE id = ?1",
                        [record.id.as_str()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(|e| {
                        storage_err(format!("cannot check for an existing version: {e}"))
                    })?;

                match existing {
                    // Idempotent: the same version with the same content is a
                    // successful no-op, so a retry after a partial failure is
                    // safe.
                    Some((checksum, _)) if checksum == record.checksum => Ok(()),
                    // A different body under an existing identifier would rewrite
                    // history, which versions exist to prevent.
                    Some((checksum, _)) => Err(storage_err(format!(
                        "version {} already exists with checksum {checksum}; \
                         versions are immutable and must not be rewritten",
                        record.id
                    ))),
                    None => Ok(()),
                }
            })
            .await?;

        // The body lands before the row. A version row without a readable body
        // would be listed but unusable, whereas an orphan body left by a crash
        // between the two steps is invisible and harmless.
        self.write_body_atomically(&path, body, CONFIG_FILE_MODE)
            .await?;

        let record = stored.clone();
        let outcome = self
            .pool
            .with_connection(move |conn| {
                conn.execute(
                    "INSERT INTO config_versions
                        (id, instance_id, sequence, source_kind,
                         source_subscription, source_rollback_from,
                         checksum, created_at, activated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT(id) DO NOTHING",
                    params![
                        record.id,
                        record.instance_id,
                        record.sequence,
                        record.source_kind,
                        record.source_subscription,
                        record.source_rollback_from,
                        record.checksum,
                        record.created_at,
                        record.activated_at,
                    ],
                )
                .map_err(|e| storage_err(format!("cannot save version: {e}")))?;
                Ok(())
            })
            .await;

        // Only clean up when this call actually created the file and failed to
        // record the version. The pre-check above already excluded the case where
        // the path belongs to an existing version.
        if outcome.is_err() {
            let _ = tokio::fs::remove_file(&path).await;
        }
        outcome
    }

    async fn active(
        &self,
        instance: &MihomoInstanceId,
    ) -> Result<Option<ConfigVersion>, PortError> {
        let key = instance.as_str().to_owned();
        let pointer = self
            .pool
            .with_connection(move |conn| {
                conn.query_row(
                    "SELECT version_id, checksum FROM config_active WHERE instance_id = ?1",
                    [key.as_str()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(|e| storage_err(format!("cannot read the active pointer: {e}")))
            })
            .await?;

        let Some((version_id, pointer_checksum)) = pointer else {
            return Ok(None);
        };

        let id = ConfigVersionId::parse(version_id.clone()).map_err(|e| {
            storage_err(format!("the active pointer names an invalid version: {e}"))
        })?;

        let version = self.get(&id).await?.ok_or_else(|| {
            storage_err(format!(
                "the active pointer names {version_id}, which has no recorded version; \
                 the pointer and the version history disagree"
            ))
        })?;

        // Cross-check the pointer against the version record. A disagreement
        // means one of them was not written, and the caller must not be told a
        // config is active on the strength of a value that contradicts itself.
        if version.checksum().as_str() != pointer_checksum {
            return Err(storage_err(format!(
                "the active pointer for {version_id} records checksum {pointer_checksum}, \
                 but the version record says {}",
                version.checksum().as_str()
            )));
        }

        // And verify the body on disk still matches, so an externally modified
        // file cannot be reported as the active configuration.
        let body = self.read_body(&version).await?;
        let actual = body.checksum();
        if actual.as_str() != pointer_checksum {
            return Err(storage_err(format!(
                "the stored body for {version_id} does not match its recorded checksum \
                 (recorded {pointer_checksum}, actual {})",
                actual.as_str()
            )));
        }

        Ok(Some(version))
    }

    async fn set_active(
        &self,
        instance: &MihomoInstanceId,
        id: &ConfigVersionId,
    ) -> Result<(), PortError> {
        let instance_key = instance.as_str().to_owned();
        let version_key = id.as_str().to_owned();
        let now = wall_clock_seconds();

        self.pool
            .with_connection(move |conn| {
                let version: Option<(String, String)> = conn
                    .query_row(
                        "SELECT instance_id, checksum FROM config_versions WHERE id = ?1",
                        [version_key.as_str()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(|e| {
                        storage_err(format!("cannot read the version to activate: {e}"))
                    })?;

                let Some((owner, checksum)) = version else {
                    // The port requires idempotency, but activating a version
                    // that does not exist cannot be a no-op: the caller would
                    // believe a config is active when nothing is.
                    return Err(storage_err(format!(
                        "cannot activate {version_key}: no such version"
                    )));
                };

                // A version belonging to another instance must not become this
                // instance's active config.
                if owner != instance_key {
                    return Err(storage_err(format!(
                        "version {version_key} belongs to instance {owner}, not {instance_key}"
                    )));
                }

                let transaction = conn
                    .transaction()
                    .map_err(|e| storage_err(format!("cannot begin activation: {e}")))?;

                // Idempotent: setting the same version twice leaves the same
                // state, including its original switch time, so a retry does not
                // make an activation look newer than it is.
                transaction
                    .execute(
                        "INSERT INTO config_active (instance_id, version_id, checksum, switched_at)
                         VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(instance_id) DO UPDATE SET
                            version_id = excluded.version_id,
                            checksum = excluded.checksum,
                            switched_at = CASE
                                WHEN config_active.version_id = excluded.version_id
                                THEN config_active.switched_at
                                ELSE excluded.switched_at
                            END",
                        params![instance_key, version_key, checksum, now],
                    )
                    .map_err(|e| storage_err(format!("cannot set the active pointer: {e}")))?;

                // Record the activation time on the version itself; this is the
                // evidence the active pointer is cross-checked against.
                transaction
                    .execute(
                        "UPDATE config_versions SET activated_at = ?1
                         WHERE id = ?2 AND activated_at IS NULL",
                        params![now, version_key],
                    )
                    .map_err(|e| storage_err(format!("cannot record the activation time: {e}")))?;

                transaction
                    .commit()
                    .map_err(|e| storage_err(format!("cannot commit activation: {e}")))?;
                Ok(())
            })
            .await
    }

    async fn read_body(&self, version: &ConfigVersion) -> Result<ConfigBody, PortError> {
        let path = self.body_path(version)?;
        let contents = tokio::fs::read_to_string(&path).await.map_err(|e| {
            storage_err(format!(
                "cannot read the body for {} at {}: {e}",
                version.id().as_str(),
                path.display()
            ))
        })?;

        let body = ConfigBody::new(contents).map_err(|e| {
            storage_err(format!(
                "stored body for {} is invalid: {e}",
                version.id().as_str()
            ))
        })?;

        // Verify on read: a body that no longer hashes to its recorded checksum
        // has been modified or truncated, and must not be handed back as if it
        // were the version that was recorded.
        let actual = body.checksum();
        if &actual != version.checksum() {
            return Err(storage_err(format!(
                "the stored body for {} does not match its checksum (recorded {}, actual {})",
                version.id().as_str(),
                version.checksum().as_str(),
                actual.as_str()
            )));
        }

        Ok(body)
    }

    async fn prune(&self, instance: &MihomoInstanceId, keep: usize) -> Result<usize, PortError> {
        let key = instance.as_str().to_owned();
        let keep = keep.max(1);

        let removed = self
            .pool
            .with_connection(move |conn| {
                let transaction = conn
                    .transaction()
                    .map_err(|e| storage_err(format!("cannot begin prune: {e}")))?;

                // Excluding the active version is done in SQL rather than by the
                // caller, because "never delete the active version" is a
                // guarantee the port states, not a convention. The pointer is
                // read inside the same transaction so it cannot change under us.
                let active: Option<String> = transaction
                    .query_row(
                        "SELECT version_id FROM config_active WHERE instance_id = ?1",
                        [key.as_str()],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|e| storage_err(format!("cannot read the active pointer: {e}")))?;

                let mut doomed = Vec::new();
                {
                    let mut statement = transaction
                        .prepare(
                            "SELECT id FROM config_versions
                             WHERE instance_id = ?1
                               AND (?2 IS NULL OR id != ?2)
                             ORDER BY sequence DESC LIMIT -1 OFFSET ?3",
                        )
                        .map_err(|e| storage_err(format!("cannot prepare prune query: {e}")))?;

                    let mapped = statement
                        .query_map(params![key, active, keep as i64], |row| {
                            row.get::<_, String>(0)
                        })
                        .map_err(|e| storage_err(format!("cannot list prunable versions: {e}")))?;

                    for entry in mapped {
                        doomed
                            .push(entry.map_err(|e| storage_err(format!("cannot read row: {e}")))?);
                    }
                }

                for id in &doomed {
                    transaction
                        .execute("DELETE FROM config_versions WHERE id = ?1", [id.as_str()])
                        .map_err(|e| storage_err(format!("cannot prune version {id}: {e}")))?;
                }

                transaction
                    .commit()
                    .map_err(|e| storage_err(format!("cannot commit prune: {e}")))?;

                Ok(doomed)
            })
            .await?;

        // Bodies are removed after the metadata commits. Doing it first would
        // leave a listed version with no body if the transaction then failed.
        // The row is already gone, so the filename is derived from the
        // identifier suffix, the same way `body_path` derives it from the label.
        for id in &removed {
            let Some(label) = label_from_id(id) else {
                continue;
            };
            let path = self.configs_dir.join(format!("{label}.yaml"));
            // The metadata is already gone, so a leftover body is invisible and
            // harmless. Failing the prune over it would misreport a successful
            // prune as a failure, so the error is deliberately dropped.
            let _ = tokio::fs::remove_file(&path).await;
        }

        Ok(removed.len())
    }
}

/// Whether a label is a bare `vNNN`, safe to use as a filename component.
fn is_safe_label(label: &str) -> bool {
    let Some(digits) = label.strip_prefix('v') else {
        return false;
    };
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// Recovers the `vNNN` label from a version identifier like `default-003`.
fn label_from_id(id: &str) -> Option<String> {
    let sequence = id.rsplit('-').next()?;
    if sequence.is_empty() || !sequence.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("v{sequence}"))
}

fn map_version_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredVersion> {
    Ok(StoredVersion {
        id: row.get(0)?,
        instance_id: row.get(1)?,
        sequence: row.get(2)?,
        source_kind: row.get(3)?,
        source_subscription: row.get(4)?,
        source_rollback_from: row.get(5)?,
        checksum: row.get(6)?,
        created_at: row.get(7)?,
        activated_at: row.get(8)?,
    })
}

/// One row of `config_versions`.
#[derive(Debug, Clone)]
struct StoredVersion {
    id: String,
    instance_id: String,
    sequence: i64,
    source_kind: String,
    source_subscription: Option<String>,
    source_rollback_from: Option<String>,
    checksum: String,
    created_at: i64,
    activated_at: Option<i64>,
}

impl StoredVersion {
    fn from_domain(version: &ConfigVersion) -> Self {
        Self {
            id: version.id().as_str().to_owned(),
            instance_id: version.instance_id().as_str().to_owned(),
            sequence: version.sequence() as i64,
            source_kind: version.source().as_str().to_owned(),
            source_subscription: version
                .source()
                .subscription_id()
                .map(|id| id.as_str().to_owned()),
            source_rollback_from: version
                .source()
                .rollback_from()
                .map(|id| id.as_str().to_owned()),
            checksum: version.checksum().as_str().to_owned(),
            created_at: version.created_at().as_unix_seconds(),
            activated_at: version.activated_at().map(Timestamp::as_unix_seconds),
        }
    }

    /// Rebuilds the version, rejecting anything unreadable.
    fn into_domain(self) -> Result<ConfigVersion, PortError> {
        let id = ConfigVersionId::parse(self.id.clone())
            .map_err(|e| storage_err(format!("version {} has an invalid id: {e}", self.id)))?;
        let instance_id = MihomoInstanceId::parse(self.instance_id.clone()).map_err(|e| {
            storage_err(format!("version {} has an invalid instance: {e}", self.id))
        })?;
        let sequence = u64::try_from(self.sequence).map_err(|_| {
            storage_err(format!(
                "version {} has an invalid sequence: {}",
                self.id, self.sequence
            ))
        })?;
        let source = ConfigSource::from_parts(
            &self.source_kind,
            self.source_subscription.as_deref(),
            self.source_rollback_from.as_deref(),
        )
        .map_err(|e| storage_err(format!("version {} has an unreadable source: {e}", self.id)))?;
        let checksum = ConfigChecksum::parse(self.checksum.clone()).map_err(|e| {
            storage_err(format!("version {} has an invalid checksum: {e}", self.id))
        })?;

        ConfigVersion::reconstitute(
            id,
            instance_id,
            sequence,
            source,
            checksum,
            Timestamp::from_unix_seconds(self.created_at),
            self.activated_at.map(Timestamp::from_unix_seconds),
        )
        .map_err(|e| storage_err(format!("cannot restore version: {e}")))
    }
}

/// The wall clock, in Unix seconds.
fn wall_clock_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Sets a path's mode, blocking.
fn set_mode_blocking(path: &Path, mode: u32) -> Result<(), PortError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| storage_err(format!("cannot set mode on {}: {e}", path.display())))
}

/// Sets a path's mode, asynchronously.
async fn set_mode(path: &Path, mode: u32) -> Result<(), PortError> {
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::Permissions::from_mode(mode)
    };
    tokio::fs::set_permissions(path, permissions)
        .await
        .map_err(|e| storage_err(format!("cannot set mode on {}: {e}", path.display())))
}

#[cfg(test)]
#[path = "configs/tests.rs"]
mod tests;
