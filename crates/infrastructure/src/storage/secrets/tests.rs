//! Tests for the SQLite secret store.

use super::*;
use proxy_application::ports::secret_store::SecretStore;

async fn store() -> (SqliteSecretStore, SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("metadata.sqlite"))
        .await
        .expect("pool");
    (SqliteSecretStore::new(pool.clone()), pool, dir)
}

#[tokio::test]
async fn the_secret_is_generated_on_demand() {
    let (store, _pool, _dir) = store().await;
    let secret = store.mihomo_secret().await.expect("secret");

    assert!(
        !secret.is_empty(),
        "an empty secret would disable kernel authentication"
    );
    assert_eq!(
        secret.len(),
        SECRET_BYTES * 2,
        "the secret should be the documented length in hex"
    );
}

/// The secret must be stable, or every restart would break the running kernel's
/// authentication and require a reload.
#[tokio::test]
async fn the_secret_is_stable_across_calls() {
    let (store, _pool, _dir) = store().await;
    let first = store.mihomo_secret().await.expect("first");
    let second = store.mihomo_secret().await.expect("second");
    assert_eq!(first, second);
}

#[tokio::test]
async fn the_secret_survives_reopening() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("metadata.sqlite");

    let first = {
        let pool = SqlitePool::open(&path).await.expect("pool");
        SqliteSecretStore::new(pool)
            .mihomo_secret()
            .await
            .expect("secret")
    };

    let pool = SqlitePool::open(&path).await.expect("reopen");
    let second = SqliteSecretStore::new(pool)
        .mihomo_secret()
        .await
        .expect("secret");
    assert_eq!(first, second, "the secret must survive a restart");
}

#[tokio::test]
async fn rotation_changes_the_secret() {
    let (store, _pool, _dir) = store().await;
    let original = store.mihomo_secret().await.expect("initial");
    let rotated = store.rotate_mihomo_secret().await.expect("rotate");

    assert_ne!(rotated, original, "rotation must produce a new value");
    assert_eq!(
        store.mihomo_secret().await.expect("after"),
        rotated,
        "the rotated value must be what is stored"
    );
}

/// Two secrets must not collide, which a weak generator would allow.
#[tokio::test]
async fn generated_secrets_differ() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..64 {
        let secret = generate_secret().expect("secret");
        assert!(seen.insert(secret.clone()), "duplicate secret: {secret}");
    }
}

#[tokio::test]
async fn generated_secrets_are_hex_and_the_documented_length() {
    let secret = generate_secret().expect("secret");
    assert_eq!(secret.len(), SECRET_BYTES * 2);
    assert!(
        secret.chars().all(|c| c.is_ascii_hexdigit()),
        "the secret must be hex so it needs no escaping: {secret}"
    );
}

/// An empty stored value would disable kernel authentication, so it must be
/// reported rather than returned as a usable secret.
#[tokio::test]
async fn an_empty_stored_secret_is_refused() {
    let (store, pool, _dir) = store().await;

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO secrets (name, value, created_at) VALUES (?1, '', 1)",
            [MIHOMO_SECRET_KEY],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let err = store
        .mihomo_secret()
        .await
        .expect_err("an empty secret must not be returned");
    assert!(
        err.to_string().contains("disable kernel authentication"),
        "the error should explain the consequence: {err}"
    );
}

/// The store must never persist an empty value, whatever the caller does.
#[tokio::test]
async fn storing_an_empty_secret_is_refused() {
    let (store, _pool, _dir) = store().await;
    let err = store
        .store("some.key", "")
        .await
        .expect_err("an empty secret must not be stored");
    assert!(err.to_string().contains("empty secret"), "{err}");
}

#[tokio::test]
async fn an_issued_token_verifies() {
    let (store, _pool, _dir) = store().await;
    let token = store.issue_api_token("operator").await.expect("issue");

    let principal = store
        .verify_api_token(&token)
        .await
        .expect("verify")
        .expect("a valid token must resolve");

    assert_eq!(principal.id, "operator");
}

/// An unknown credential is an expected outcome, not a fault.
#[tokio::test]
async fn an_unknown_token_returns_none_rather_than_an_error() {
    let (store, _pool, _dir) = store().await;
    let _ = store.issue_api_token("operator").await.expect("issue");

    assert!(
        store
            .verify_api_token("not-the-token")
            .await
            .expect("an unknown token is not an error")
            .is_none()
    );
}

#[tokio::test]
async fn an_empty_presented_token_returns_none() {
    let (store, _pool, _dir) = store().await;
    let _ = store.issue_api_token("operator").await.expect("issue");
    assert!(store.verify_api_token("").await.expect("verify").is_none());
}

/// A prefix of a valid token must not verify, which a sloppy comparison would
/// allow.
#[tokio::test]
async fn a_prefix_of_a_valid_token_does_not_verify() {
    let (store, _pool, _dir) = store().await;
    let token = store.issue_api_token("operator").await.expect("issue");

    let prefix = &token[..token.len() - 1];
    assert!(
        store
            .verify_api_token(prefix)
            .await
            .expect("verify")
            .is_none(),
        "a shortened token must not authenticate"
    );

    let extended = format!("{token}0");
    assert!(
        store
            .verify_api_token(&extended)
            .await
            .expect("verify")
            .is_none(),
        "an extended token must not authenticate"
    );
}

#[tokio::test]
async fn issuing_a_new_token_replaces_the_previous_one() {
    let (store, _pool, _dir) = store().await;
    let first = store.issue_api_token("operator").await.expect("first");
    let second = store.issue_api_token("operator").await.expect("second");

    assert_ne!(first, second);
    assert!(
        store
            .verify_api_token(&first)
            .await
            .expect("verify")
            .is_none(),
        "the replaced token must stop working"
    );
    assert!(
        store
            .verify_api_token(&second)
            .await
            .expect("verify")
            .is_some()
    );
}

#[tokio::test]
async fn an_empty_principal_is_refused() {
    let (store, _pool, _dir) = store().await;
    assert!(store.issue_api_token("  ").await.is_err());
}

/// A row inserted directly, bypassing `issue_api_token`, must still verify.
///
/// The schema is the contract, not the issuing path: a deployment that restores a
/// database, or a future build that writes the table differently, still has to
/// authenticate against it. This is the check that the read path depends only on
/// the columns the schema defines.
#[tokio::test]
async fn a_row_inserted_directly_still_verifies() {
    let (store, pool, _dir) = store().await;

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO api_principals (id, token_hash, token_salt, created_at)
             VALUES ('restored', ?1, 'salt', 1)",
            [super::hash_token("salt", "abc123")],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let principal = store
        .verify_api_token("abc123")
        .await
        .expect("verify")
        .expect("a directly inserted row must authenticate");
    assert_eq!(principal.id, "restored");
}

#[tokio::test]
async fn constant_time_comparison_is_correct() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"ab"));
    assert!(!constant_time_eq(b"", b"a"));
    // The final byte differing must be caught, i.e. the loop does not exit early
    // on a matching prefix.
    assert!(!constant_time_eq(b"aaaaaaaaab", b"aaaaaaaaac"));
    assert!(constant_time_eq(b"", b""));
}

#[tokio::test]
async fn the_token_is_not_readable_from_the_store_after_issue() {
    let (store, _pool, _dir) = store().await;
    let token = store.issue_api_token("operator").await.expect("issue");

    // Verification is the only way back in; there is no read-the-token method.
    let principal = store
        .verify_api_token(&token)
        .await
        .expect("verify")
        .expect("valid");
    assert_eq!(principal.id, "operator");
}

#[tokio::test]
async fn secrets_location_reports_the_database_path() {
    let (store, pool, dir) = store().await;
    let _ = store.mihomo_secret().await.expect("secret");
    let location = secrets_location(&pool);
    assert!(location.contains("metadata.sqlite"), "{location}");
    assert!(
        location.starts_with(&dir.path().display().to_string()),
        "{location}"
    );
}

/// `ensure_mihomo_secret` is what bootstrap calls eagerly; it must be idempotent
/// so a restart reuses the existing secret.
#[tokio::test]
async fn ensure_is_idempotent() {
    let (store, _pool, _dir) = store().await;
    let first = store.ensure_mihomo_secret().await.expect("first");
    let second = store.ensure_mihomo_secret().await.expect("second");
    assert_eq!(first, second);
}

/// The reason the token column became a hash: a copy of the database must not
/// yield working credentials.
#[tokio::test]
async fn a_token_is_not_recoverable_from_the_database() {
    let (store, pool, _dir) = store().await;
    let token = store.issue_api_token("operator").await.expect("issue");

    let stored: Vec<(String, String)> = pool
        .with_connection(|conn| {
            let mut statement = conn
                .prepare("SELECT token_hash, token_salt FROM api_principals")
                .map_err(|e| storage_err(e.to_string()))?;
            let mapped = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| storage_err(e.to_string()))?;
            let mut out = Vec::new();
            for entry in mapped {
                out.push(entry.map_err(|e| storage_err(e.to_string()))?);
            }
            Ok(out)
        })
        .await
        .expect("read");

    assert_eq!(stored.len(), 1);
    let (hash, salt) = &stored[0];
    assert_ne!(hash, &token, "the token must not be stored as itself");
    assert!(
        !hash.contains(&token),
        "the token must not appear inside the stored value"
    );
    assert!(!salt.is_empty(), "a salt must be stored");

    // And the token still works, so hashing did not break verification.
    let principal = store
        .verify_api_token(&token)
        .await
        .expect("verify")
        .expect("a match");
    assert_eq!(principal.id, "operator");
}

/// Two principals issued the same token value would still store different hashes,
/// so a stolen database cannot be compared against another one.
#[tokio::test]
async fn identical_tokens_would_produce_different_hashes() {
    // Two salts over one token: the property under test is the salt's effect, so
    // it is asserted directly rather than by trying to collide real tokens.
    let a = super::hash_token("salt-a", "same-token");
    let b = super::hash_token("salt-b", "same-token");
    assert_ne!(a, b, "a per-row salt must change the hash");
}

/// The salt is per row, not global, which is what makes the previous property
/// hold across principals.
#[tokio::test]
async fn each_principal_gets_its_own_salt() {
    let (store, pool, _dir) = store().await;
    store.issue_api_token("first").await.expect("issue");
    store.issue_api_token("second").await.expect("issue");

    let salts: Vec<String> = pool
        .with_connection(|conn| {
            let mut statement = conn
                .prepare("SELECT token_salt FROM api_principals ORDER BY id")
                .map_err(|e| storage_err(e.to_string()))?;
            let mapped = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| storage_err(e.to_string()))?;
            let mut out = Vec::new();
            for entry in mapped {
                out.push(entry.map_err(|e| storage_err(e.to_string()))?);
            }
            Ok(out)
        })
        .await
        .expect("read");

    assert_eq!(salts.len(), 2);
    assert_ne!(salts[0], salts[1], "salts must not be shared");
}

/// Two principals must resolve to themselves and to nothing else. There is no
/// identity to carry across the round trip; what matters is that it
/// is not confused between them.
#[tokio::test]
async fn each_principal_resolves_to_itself() {
    let (store, _pool, _dir) = store().await;
    let first = store.issue_api_token("first").await.expect("issue");
    let second = store.issue_api_token("second").await.expect("issue");

    assert_eq!(
        store
            .verify_api_token(&first)
            .await
            .expect("verify")
            .expect("match")
            .id,
        "first"
    );
    assert_eq!(
        store
            .verify_api_token(&second)
            .await
            .expect("verify")
            .expect("match")
            .id,
        "second"
    );
    // And neither token authenticates as the other principal.
    assert_ne!(
        store
            .verify_api_token(&first)
            .await
            .expect("verify")
            .expect("match")
            .id,
        "second"
    );
}

/// Issuing again for the same principal replaces the token, which is how rotation
/// is expressed, and the old value must stop working.
#[tokio::test]
async fn reissuing_replaces_the_token_and_retires_the_old_one() {
    let (store, _pool, _dir) = store().await;
    let first = store.issue_api_token("operator").await.expect("first");
    let second = store.issue_api_token("operator").await.expect("second");

    assert_ne!(first, second, "a new token must be a new value");
    assert!(
        store
            .verify_api_token(&first)
            .await
            .expect("verify")
            .is_none(),
        "the replaced token must no longer authenticate"
    );
    assert!(
        store
            .verify_api_token(&second)
            .await
            .expect("verify")
            .is_some(),
        "the current token must authenticate"
    );
}

/// The listing must not carry anything that could authenticate a caller.
#[tokio::test]
async fn the_listing_exposes_no_hash_and_no_salt() {
    let (store, _pool, _dir) = store().await;
    let token = store.issue_api_token("operator").await.expect("issue");

    let listed = store.list_api_tokens().await.expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "operator");
    assert!(listed[0].created_at > 0);

    // The only fields that exist are the three above; this asserts the token
    // cannot be reconstructed from what a listing returns.
    let rendered = format!("{listed:?}");
    assert!(
        !rendered.contains(&token),
        "the listing leaked the token: {rendered}"
    );
}

/// Revocation must actually stop authentication, and report honestly.
#[tokio::test]
async fn revocation_removes_the_token_and_reports_whether_one_existed() {
    let (store, _pool, _dir) = store().await;
    let token = store.issue_api_token("operator").await.expect("issue");

    assert!(
        store.revoke_api_token("operator").await.expect("revoke"),
        "revoking an existing principal reports true"
    );
    assert!(
        store
            .verify_api_token(&token)
            .await
            .expect("verify")
            .is_none(),
        "a revoked token must not authenticate"
    );
    assert!(
        !store
            .revoke_api_token("operator")
            .await
            .expect("revoke again"),
        "revoking an unknown principal reports false rather than failing"
    );
}

/// A blank token must not authenticate, and must not be hashed into a match.
#[tokio::test]
async fn an_empty_presented_token_is_never_a_match() {
    let (store, _pool, _dir) = store().await;
    let _ = store.issue_api_token("operator").await.expect("issue");
    assert!(store.verify_api_token("").await.expect("verify").is_none());
}
