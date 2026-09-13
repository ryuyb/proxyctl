//! Tests for the file-backed config repository.

use super::*;
use proxy_application::ports::config_repository::ConfigRepository;
use proxy_domain::shared::id::SubscriptionId;

struct Fixture {
    repo: FileConfigRepository,
    pool: SqlitePool,
    /// Held so the temporary directory outlives the repository. Its contents are
    /// reached through the repository, never by path, so it is deliberately not
    /// read here.
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("metadata.sqlite"))
        .await
        .expect("pool");
    let repo = FileConfigRepository::new(pool.clone(), dir.path().join("configs"))
        .await
        .expect("repo");
    Fixture {
        repo,
        pool,
        _dir: dir,
    }
}

fn instance() -> MihomoInstanceId {
    MihomoInstanceId::parse("default").expect("valid")
}

fn config_body(text: &str) -> ConfigBody {
    ConfigBody::new(text).expect("valid body")
}

/// Builds a version the way the application does: allocate a sequence, then
/// derive the identifier from the instance and that sequence.
async fn make_version(
    repo: &FileConfigRepository,
    text: &str,
    source: ConfigSource,
) -> (ConfigVersion, ConfigBody) {
    let sequence = repo.next_sequence(&instance()).await.expect("sequence");
    let body = config_body(text);
    let version = ConfigVersion::record(
        ConfigVersionId::parse(format!("default-{sequence:03}")).expect("valid"),
        instance(),
        sequence,
        source,
        body.checksum(),
        Timestamp::from_unix_seconds(1_700_000_000 + sequence as i64),
    );
    (version, body)
}

#[tokio::test]
async fn an_empty_repository_has_no_versions() {
    let f = fixture().await;
    assert!(f.repo.list(&instance(), 10).await.expect("list").is_empty());
    assert!(f.repo.active(&instance()).await.expect("active").is_none());
}

#[tokio::test]
async fn a_saved_version_round_trips_with_its_body() {
    let f = fixture().await;
    let (version, body) = make_version(&f.repo, "mixed-port: 7890\n", ConfigSource::Manual).await;
    f.repo.save(&version, &body).await.expect("save");

    let loaded = f
        .repo
        .get(version.id())
        .await
        .expect("get")
        .expect("present");
    assert_eq!(loaded, version);

    let read_back = f.repo.read_body(&loaded).await.expect("read body");
    assert_eq!(read_back.as_str(), "mixed-port: 7890\n");
}

/// Sequences must be strictly increasing so a new version can never collide with
/// an existing one.
#[tokio::test]
async fn sequence_numbers_are_monotonic_and_start_at_one() {
    let f = fixture().await;
    let mut seen = Vec::new();
    for _ in 0..5 {
        seen.push(f.repo.next_sequence(&instance()).await.expect("sequence"));
    }
    assert_eq!(seen, vec![1, 2, 3, 4, 5]);
}

#[tokio::test]
async fn sequences_are_allocated_atomically_under_concurrency() {
    let f = fixture().await;
    let repo = f.repo.clone();

    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let repo = repo.clone();
            tokio::spawn(async move { repo.next_sequence(&instance()).await })
        })
        .collect();

    let mut allocated = Vec::new();
    for task in tasks {
        allocated.push(task.await.expect("join").expect("sequence"));
    }
    allocated.sort_unstable();
    allocated.dedup();
    assert_eq!(
        allocated.len(),
        8,
        "concurrent allocations must not repeat: {allocated:?}"
    );
}

#[tokio::test]
async fn sequences_are_per_instance() {
    let f = fixture().await;
    assert_eq!(f.repo.next_sequence(&instance()).await.expect("a"), 1);
    let other = MihomoInstanceId::parse("second").expect("valid");
    assert_eq!(f.repo.next_sequence(&other).await.expect("b"), 1);
}

/// Idempotency: retrying a save that already succeeded must be a no-op, which is
/// what makes the recovery path safe.
#[tokio::test]
async fn saving_the_same_version_twice_is_a_no_op() {
    let f = fixture().await;
    let (version, body) = make_version(&f.repo, "mode: rule\n", ConfigSource::Manual).await;

    f.repo.save(&version, &body).await.expect("first");
    f.repo
        .save(&version, &body)
        .await
        .expect("second must be a no-op");

    assert_eq!(f.repo.list(&instance(), 10).await.expect("list").len(), 1);
}

/// Versions are immutable, so a different body under an existing identifier must
/// be refused rather than silently overwriting history.
#[tokio::test]
async fn rewriting_an_existing_version_is_refused() {
    let f = fixture().await;
    let (version, body) = make_version(&f.repo, "mode: rule\n", ConfigSource::Manual).await;
    f.repo.save(&version, &body).await.expect("save");

    // Same identifier, different content. A caller would normally build a fresh
    // version for this, so the body's checksum must match the version being
    // saved; otherwise the earlier checksum guard fires and this test would
    // never reach the immutability rule it is meant to cover.
    let different = config_body("mode: global\n");
    let rewritten = ConfigVersion::record(
        version.id().clone(),
        version.instance_id().clone(),
        version.sequence(),
        ConfigSource::Manual,
        different.checksum(),
        version.created_at(),
    );
    let err = f
        .repo
        .save(&rewritten, &different)
        .await
        .expect_err("rewriting a version must be refused");
    assert!(err.to_string().contains("immutable"), "{err}");

    // The original must be untouched.
    let loaded = f
        .repo
        .get(version.id())
        .await
        .expect("get")
        .expect("present");
    assert_eq!(
        f.repo.read_body(&loaded).await.expect("body").as_str(),
        "mode: rule\n"
    );
}

/// A caller-supplied checksum that does not match the bytes is a bug upstream,
/// and storing it would make every later verification fail.
#[tokio::test]
async fn a_mismatched_checksum_is_refused() {
    let f = fixture().await;
    let sequence = f.repo.next_sequence(&instance()).await.expect("sequence");
    let version = ConfigVersion::record(
        ConfigVersionId::parse(format!("default-{sequence:03}")).expect("valid"),
        instance(),
        sequence,
        ConfigSource::Manual,
        ConfigChecksum::from_digest(999),
        Timestamp::from_unix_seconds(1),
    );

    let err = f
        .repo
        .save(&version, &config_body("mode: rule\n"))
        .await
        .expect_err("a mismatched checksum must be refused");
    assert!(err.to_string().contains("mismatched checksum"), "{err}");
    assert!(
        f.repo.get(version.id()).await.expect("get").is_none(),
        "nothing must be recorded"
    );
}

/// A rejected save must not leave the body behind, or the next attempt at that
/// sequence would find a file for a version that does not exist.
#[tokio::test]
async fn a_rejected_save_leaves_no_body_behind() {
    let f = fixture().await;
    let (version, body) = make_version(&f.repo, "mode: rule\n", ConfigSource::Manual).await;
    f.repo.save(&version, &body).await.expect("save");

    let path = f.repo.body_path(&version).expect("path");
    assert!(path.exists(), "the first save must write a body");

    // The rejected rewrite must not change the file.
    let _ = f.repo.save(&version, &config_body("different\n")).await;
    assert_eq!(
        tokio::fs::read_to_string(&path).await.expect("read"),
        "mode: rule\n",
        "a rejected save must not alter the stored body"
    );
}

#[tokio::test]
async fn activating_a_version_records_it_as_active() {
    let f = fixture().await;
    let (version, body) = make_version(&f.repo, "mode: rule\n", ConfigSource::Manual).await;
    f.repo.save(&version, &body).await.expect("save");

    f.repo
        .set_active(&instance(), version.id())
        .await
        .expect("activate");

    let active = f
        .repo
        .active(&instance())
        .await
        .expect("active")
        .expect("some");
    assert_eq!(active.id(), version.id());
    assert!(active.is_active());
    assert!(active.activated_at().is_some());
}

#[tokio::test]
async fn activation_is_idempotent_and_keeps_the_original_time() {
    let f = fixture().await;
    let (version, body) = make_version(&f.repo, "mode: rule\n", ConfigSource::Manual).await;
    f.repo.save(&version, &body).await.expect("save");

    f.repo
        .set_active(&instance(), version.id())
        .await
        .expect("first");
    let first = f
        .repo
        .active(&instance())
        .await
        .expect("active")
        .expect("some");

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    f.repo
        .set_active(&instance(), version.id())
        .await
        .expect("second");
    let second = f
        .repo
        .active(&instance())
        .await
        .expect("active")
        .expect("some");

    assert_eq!(first.id(), second.id());
    assert_eq!(
        first.activated_at(),
        second.activated_at(),
        "re-activating the same version must not make it look newer"
    );
}

#[tokio::test]
async fn activating_an_unknown_version_is_refused() {
    let f = fixture().await;
    let id = ConfigVersionId::parse("default-999").expect("valid");

    let err = f
        .repo
        .set_active(&instance(), &id)
        .await
        .expect_err("activating a missing version must fail");
    assert!(err.to_string().contains("no such version"), "{err}");
}

/// A version belonging to another instance must not become this instance's
/// active config.
#[tokio::test]
async fn activating_another_instances_version_is_refused() {
    let f = fixture().await;
    let other = MihomoInstanceId::parse("second").expect("valid");
    let sequence = f.repo.next_sequence(&other).await.expect("sequence");
    let body = config_body("mode: rule\n");
    let version = ConfigVersion::record(
        ConfigVersionId::parse(format!("second-{sequence:03}")).expect("valid"),
        other.clone(),
        sequence,
        ConfigSource::Manual,
        body.checksum(),
        Timestamp::from_unix_seconds(1),
    );
    f.repo.save(&version, &body).await.expect("save");

    let err = f
        .repo
        .set_active(&instance(), version.id())
        .await
        .expect_err("cross-instance activation must fail");
    assert!(err.to_string().contains("belongs to instance"), "{err}");
}

/// The active pointer is cross-checked against the version record: a row that
/// contradicts itself must not be reported as a working active config.
#[tokio::test]
async fn a_pointer_that_contradicts_its_version_is_reported() {
    let f = fixture().await;
    let (version, body) = make_version(&f.repo, "mode: rule\n", ConfigSource::Manual).await;
    f.repo.save(&version, &body).await.expect("save");
    f.repo
        .set_active(&instance(), version.id())
        .await
        .expect("activate");

    // Tamper with the pointer, as a partial write might.
    f.pool
        .with_connection(|conn| {
            conn.execute(
                "UPDATE config_active SET checksum = 'fnv1a64:deadbeefdeadbeef'",
                [],
            )
            .map_err(|e| storage_err(e.to_string()))?;
            Ok(())
        })
        .await
        .expect("tamper");

    let err = f
        .repo
        .active(&instance())
        .await
        .expect_err("a contradictory pointer must be reported");
    assert!(err.to_string().contains("version record says"), "{err}");
}

/// An externally modified body must not be handed back as the recorded version.
#[tokio::test]
async fn a_modified_body_is_detected_on_read() {
    let f = fixture().await;
    let (version, body) = make_version(&f.repo, "mode: rule\n", ConfigSource::Manual).await;
    f.repo.save(&version, &body).await.expect("save");

    // Simulate an operator or another process editing the file.
    let path = f.repo.body_path(&version).expect("path");
    tokio::fs::write(&path, "mode: global\n")
        .await
        .expect("tamper");

    let err = f
        .repo
        .read_body(&version)
        .await
        .expect_err("a modified body must be detected");
    assert!(
        err.to_string().contains("does not match its checksum"),
        "{err}"
    );
}

#[tokio::test]
async fn a_missing_body_is_reported() {
    let f = fixture().await;
    let (version, body) = make_version(&f.repo, "mode: rule\n", ConfigSource::Manual).await;
    f.repo.save(&version, &body).await.expect("save");

    let path = f.repo.body_path(&version).expect("path");
    tokio::fs::remove_file(&path).await.expect("remove");

    assert!(f.repo.read_body(&version).await.is_err());
}

/// The port states prune must never delete the active version, so this asserts
/// it against the storage layer rather than trusting the caller.
#[tokio::test]
async fn prune_never_deletes_the_active_version() {
    let f = fixture().await;

    let mut versions = Vec::new();
    for i in 0..6 {
        let (version, body) = make_version(
            &f.repo,
            &format!("mode: rule\n# {i}\n"),
            ConfigSource::Manual,
        )
        .await;
        f.repo.save(&version, &body).await.expect("save");
        versions.push(version);
    }

    // Activate the oldest, which is exactly the one a naive "keep newest N"
    // prune would remove first.
    let oldest = &versions[0];
    f.repo
        .set_active(&instance(), oldest.id())
        .await
        .expect("activate");

    let removed = f.repo.prune(&instance(), 2).await.expect("prune");
    assert!(removed > 0, "prune should have removed something");

    let active = f
        .repo
        .active(&instance())
        .await
        .expect("active")
        .expect("the active version must survive");
    assert_eq!(active.id(), oldest.id());
}

#[tokio::test]
async fn prune_keeps_the_requested_number_of_recent_versions() {
    let f = fixture().await;
    for i in 0..5 {
        let (version, body) = make_version(
            &f.repo,
            &format!("mode: rule\n# {i}\n"),
            ConfigSource::Manual,
        )
        .await;
        f.repo.save(&version, &body).await.expect("save");
    }

    f.repo.prune(&instance(), 2).await.expect("prune");
    let remaining = f.repo.list(&instance(), 100).await.expect("list");
    assert_eq!(remaining.len(), 2);
    // And they are the newest two.
    assert_eq!(remaining[0].sequence(), 5);
    assert_eq!(remaining[1].sequence(), 4);
}

#[tokio::test]
async fn versions_are_listed_newest_first() {
    let f = fixture().await;
    for i in 0..3 {
        let (version, body) = make_version(
            &f.repo,
            &format!("mode: rule\n# {i}\n"),
            ConfigSource::Manual,
        )
        .await;
        f.repo.save(&version, &body).await.expect("save");
    }

    let sequences: Vec<u64> = f
        .repo
        .list(&instance(), 10)
        .await
        .expect("list")
        .iter()
        .map(ConfigVersion::sequence)
        .collect();
    assert_eq!(sequences, vec![3, 2, 1]);
}

#[tokio::test]
async fn the_list_limit_is_respected() {
    let f = fixture().await;
    for i in 0..5 {
        let (version, body) = make_version(
            &f.repo,
            &format!("mode: rule\n# {i}\n"),
            ConfigSource::Manual,
        )
        .await;
        f.repo.save(&version, &body).await.expect("save");
    }
    assert_eq!(f.repo.list(&instance(), 2).await.expect("list").len(), 2);
}

/// Provenance must survive storage, including the variants that carry data.
#[tokio::test]
async fn every_source_kind_round_trips() {
    let f = fixture().await;
    let sources = [
        ConfigSource::Subscription(SubscriptionId::parse("sub-1").expect("valid")),
        ConfigSource::Manual,
        ConfigSource::Imported,
        ConfigSource::Generated,
        ConfigSource::Rollback {
            from: ConfigVersionId::parse("default-001").expect("valid"),
        },
    ];

    for (i, source) in sources.into_iter().enumerate() {
        let (version, body) = make_version(
            &f.repo,
            &format!("mode: rule\n# source {i}\n"),
            source.clone(),
        )
        .await;
        f.repo.save(&version, &body).await.expect("save");

        let loaded = f
            .repo
            .get(version.id())
            .await
            .expect("get")
            .expect("present");
        assert_eq!(
            loaded.source(),
            &source,
            "{} must round trip",
            source.as_str()
        );
    }
}

/// Versions must survive a restart, or a rollback would have no history.
#[tokio::test]
async fn versions_survive_reopening() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("metadata.sqlite");
    let configs = dir.path().join("configs");

    let id = {
        let pool = SqlitePool::open(&path).await.expect("pool");
        let repo = FileConfigRepository::new(pool, &configs)
            .await
            .expect("repo");
        let (version, body) = make_version(&repo, "mode: rule\n", ConfigSource::Manual).await;
        repo.save(&version, &body).await.expect("save");
        repo.set_active(&instance(), version.id())
            .await
            .expect("activate");
        version.id().clone()
    };

    let pool = SqlitePool::open(&path).await.expect("reopen");
    let repo = FileConfigRepository::new(pool, &configs)
        .await
        .expect("repo");

    assert_eq!(
        repo.get(&id).await.expect("get").expect("present").id(),
        &id
    );
    let active = repo
        .active(&instance())
        .await
        .expect("active")
        .expect("some");
    assert_eq!(
        active.id(),
        &id,
        "the active pointer must survive a restart"
    );
}

/// The active body must be verifiable after a restart, since that is what
/// recovery does.
#[tokio::test]
async fn the_active_body_is_readable_after_reopening() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("metadata.sqlite");
    let configs = dir.path().join("configs");

    {
        let pool = SqlitePool::open(&path).await.expect("pool");
        let repo = FileConfigRepository::new(pool, &configs)
            .await
            .expect("repo");
        let (version, body) = make_version(&repo, "mode: rule\n", ConfigSource::Manual).await;
        repo.save(&version, &body).await.expect("save");
        repo.set_active(&instance(), version.id())
            .await
            .expect("activate");
    }

    let pool = SqlitePool::open(&path).await.expect("reopen");
    let repo = FileConfigRepository::new(pool, &configs)
        .await
        .expect("repo");
    let active = repo
        .active(&instance())
        .await
        .expect("active")
        .expect("some");
    assert_eq!(
        repo.read_body(&active).await.expect("body").as_str(),
        "mode: rule\n"
    );
}

/// A stored body is readable and writable by any local user, by decision.
///
/// It carries credentials, which is why it was once unreadable to others; the
/// reasoning for opening it is recorded on [`CONFIG_FILE_MODE`] and in
/// `AGENTS.md`. The mode is asserted so a change to it is a deliberate act rather
/// than a drift nobody notices, and the comment says plainly that this is a risk
/// accepted rather than a property protected.
#[tokio::test]
async fn stored_bodies_are_open_to_local_users() {
    use std::os::unix::fs::PermissionsExt;

    let f = fixture().await;
    let (version, body) = make_version(&f.repo, "mode: rule\n", ConfigSource::Manual).await;
    f.repo.save(&version, &body).await.expect("save");

    let path = f.repo.body_path(&version).expect("path");
    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o022,
        0o022,
        "a stored body is world-writable by decision (mode {mode:o})"
    );
    assert_eq!(mode & 0o777, CONFIG_FILE_MODE, "unexpected mode {mode:o}");
}

/// An unwritable directory must be reported, and no partial state left behind.
#[tokio::test]
async fn a_save_into_a_missing_directory_is_reported() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("metadata.sqlite"))
        .await
        .expect("pool");
    let repo = FileConfigRepository::new(pool, dir.path().join("configs"))
        .await
        .expect("repo");

    let (version, body) = make_version(&repo, "mode: rule\n", ConfigSource::Manual).await;

    // Remove the directory out from under the repository.
    tokio::fs::remove_dir_all(repo.configs_dir())
        .await
        .expect("remove dir");

    let err = repo
        .save(&version, &body)
        .await
        .expect_err("writing into a missing directory must fail");
    assert!(err.to_string().contains("cannot"), "{err}");
}

/// The directory is created on construction with restrictive permissions, since
/// it holds files containing credentials.
#[tokio::test]
async fn the_configs_directory_is_created_with_restrictive_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("metadata.sqlite"))
        .await
        .expect("pool");
    let configs = dir.path().join("configs");
    let _repo = FileConfigRepository::new(pool, &configs)
        .await
        .expect("repo");

    let mode = std::fs::metadata(&configs)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, DIRECTORY_MODE, "unexpected mode {mode:o}");
}

#[tokio::test]
async fn an_unsafe_version_label_cannot_escape_the_directory() {
    // The label is derived from the sequence, but the guard is what makes a
    // crafted identifier unable to produce a path outside the configs directory.
    assert!(is_safe_label("v001"));
    assert!(is_safe_label("v12345"));
    assert!(!is_safe_label("v"));
    assert!(!is_safe_label("../../etc/passwd"));
    assert!(!is_safe_label("v1/../../x"));
    assert!(!is_safe_label("v1.yaml"));
    assert!(!is_safe_label(""));
}
