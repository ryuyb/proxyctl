//! Tests for the config validator.
//!
//! The tests split in two. The pure ones (syntax, whitelist) run anywhere. The
//! kernel-backed ones need a real `mihomo` binary and are gated on
//! `PROXYCTL_TEST_BINARY`, because the whole reason this adapter exists is the
//! kernel's *measured* behaviour — a mock would only restate the assumption.

use super::*;
use proxy_application::ports::config_validator::ConfigValidator;

fn body(text: &str) -> ConfigBody {
    ConfigBody::new(text).expect("valid body")
}

/// The kernel binary, when the environment supplies one.
fn test_binary() -> Option<PathBuf> {
    std::env::var("PROXYCTL_TEST_BINARY")
        .ok()
        .map(PathBuf::from)
}

/// Builds a validator with a scratch directory under `dir`.
async fn validator(dir: &Path, with_binary: bool) -> KernelConfigValidator {
    let binary = if with_binary {
        test_binary().unwrap_or_else(|| PathBuf::from("/nonexistent/mihomo"))
    } else {
        PathBuf::from("/nonexistent/mihomo")
    };
    KernelConfigValidator::new(binary, dir.join("data"), dir.join("scratch"))
}

// ---------------------------------------------------------------- syntax (L1)

#[tokio::test]
async fn valid_yaml_passes_the_syntax_layer() {
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), false).await;

    let outcome = v
        .validate_syntax(&body("mixed-port: 7890\nmode: rule\n"))
        .await
        .expect("syntax");
    assert!(outcome.is_passed(), "{outcome:?}");
}

#[tokio::test]
async fn malformed_yaml_fails_the_syntax_layer() {
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), false).await;

    let outcome = v
        .validate_syntax(&body("mixed-port: [unclosed\n"))
        .await
        .expect("syntax");
    assert!(
        outcome.is_failed(),
        "unterminated flow must fail: {outcome:?}"
    );
}

/// The syntax layer must not invoke the kernel, because the kernel would create
/// state for a document that is not even parseable.
#[tokio::test]
async fn the_syntax_layer_does_not_run_the_kernel() {
    let dir = tempfile::tempdir().expect("dir");
    // A missing binary: if syntax called it, this would error rather than pass.
    let v = validator(dir.path(), false).await;
    let outcome = v
        .validate_syntax(&body("mode: rule\n"))
        .await
        .expect("syntax");
    assert!(outcome.is_passed());
}

#[tokio::test]
async fn an_empty_document_is_not_valid_yaml_mapping() {
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), false).await;
    // A bare scalar parses (as a string) but is not a config; the kernel check
    // is what rejects it. Syntax alone should not panic.
    let outcome = v
        .validate_syntax(&body("just a string"))
        .await
        .expect("syntax");
    assert!(outcome.is_passed(), "a scalar is valid YAML: {outcome:?}");
}

// ------------------------------------------------------- whitelist (pure)

#[test]
fn known_top_level_keys_are_accepted() {
    let document = body(
        "mixed-port: 7890\nmode: rule\nlog-level: info\nallow-lan: false\n\
         external-controller: 127.0.0.1:9090\nsecret: abc\ndns:\n  enable: true\n\
         proxies: []\nproxy-groups: []\nrules:\n  - MATCH,DIRECT\n",
    );
    assert_eq!(unknown_fields(&document), Vec::<String>::new());
}

/// The case the kernel cannot catch: a key the kernel silently ignores, so the
/// config does not do what its author wrote.
#[test]
fn a_misspelled_top_level_key_is_reported() {
    let document = body("mixed-portt: 7890\nmode: rule\n");
    let unknown = unknown_fields(&document);
    assert_eq!(unknown, vec!["mixed-portt".to_owned()]);
}

/// A typo *inside* a section is as harmful as one at the top level, so nested
/// keys are checked too.
#[test]
fn a_misspelled_nested_key_is_reported_with_its_path() {
    let document = body("dns:\n  enablee: true\n");
    let unknown = unknown_fields(&document);
    assert_eq!(unknown, vec!["dns.enablee".to_owned()]);
}

#[test]
fn nested_keys_of_several_sections_are_checked() {
    assert_eq!(
        unknown_fields(&body("tun:\n  enable: true\n  devic: tun0\n")),
        vec!["tun.devic".to_owned()]
    );
    assert_eq!(
        unknown_fields(&body("sniffer:\n  enable: true\n  nope: 1\n")),
        vec!["sniffer.nope".to_owned()]
    );
    assert_eq!(
        unknown_fields(&body("profile:\n  store-selected: true\n  store-x: 1\n")),
        vec!["profile.store-x".to_owned()]
    );
}

/// A section with no known shape must not produce false positives just because
/// its keys are not listed.
#[test]
fn an_unchecked_section_is_not_walked() {
    // `proxy-providers` is a map of user-chosen names, so any key is legitimate.
    let document = body("proxy-providers:\n  my-provider:\n    type: http\n");
    assert_eq!(unknown_fields(&document), Vec::<String>::new());
}

#[test]
fn unknown_fields_are_sorted_and_deduplicated() {
    let document = body("zzz: 1\naaa: 1\n");
    assert_eq!(
        unknown_fields(&document),
        vec!["aaa".to_owned(), "zzz".to_owned()]
    );
}

#[test]
fn an_unparseable_document_yields_no_unknown_fields() {
    // Syntax is reported by its own layer, so this must not double-report.
    assert_eq!(
        unknown_fields(&body("a: [unclosed\n")),
        Vec::<String>::new()
    );
}

#[test]
fn a_non_mapping_document_yields_no_unknown_fields() {
    assert_eq!(unknown_fields(&body("just a scalar")), Vec::<String>::new());
}

/// The generated list must be non-trivial, or the check silently stops working.
#[test]
fn the_whitelist_is_populated() {
    assert!(
        whitelist::TOP_LEVEL.len() > 50,
        "the whitelist looks truncated: {} entries",
        whitelist::TOP_LEVEL.len()
    );
    for required in ["mixed-port", "mode", "rules", "proxies", "tun", "dns"] {
        assert!(
            whitelist::TOP_LEVEL.contains(&required),
            "{required} must be whitelisted"
        );
    }
    assert!(!whitelist::SOURCE_TAG.is_empty());
}

/// Every section listed for nested checking must actually have keys.
#[test]
fn every_checked_section_has_keys() {
    assert!(!whitelist::SECTIONS.is_empty());
    for (name, keys) in whitelist::SECTIONS {
        assert!(!keys.is_empty(), "section {name} has no keys");
    }
}

// ------------------------------------------------------------ geodata (L0)

#[test]
fn geo_rules_are_detected() {
    let dir = Path::new("/tmp");
    let v = KernelConfigValidator::new("/bin/true", dir, dir);

    assert!(v.requires_geodata(&body("rules:\n  - GEOIP,CN,DIRECT\n")));
    assert!(v.requires_geodata(&body("rules:\n  - GEOSITE,cn,DIRECT\n")));
    assert!(v.requires_geodata(&body("geodata-mode: true\n")));
    assert!(!v.requires_geodata(&body("rules:\n  - MATCH,DIRECT\n")));
}

/// A rule-provider URL containing the word must not be mistaken for a geo rule.
#[test]
fn a_url_mentioning_geo_is_not_a_geo_rule() {
    let dir = Path::new("/tmp");
    let v = KernelConfigValidator::new("/bin/true", dir, dir);
    assert!(!v.requires_geodata(&body(
        "rule-providers:\n  x:\n    url: https://example.com/geodata/list.yaml\n"
    )));
}

/// A config needing uncached geo data while offline is *skipped*, not failed:
/// the problem is the host, not the document.
#[tokio::test]
async fn preflight_skips_geo_checks_when_offline() {
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), false).await;

    let context = PreflightContext {
        desired_ports: Vec::new(),
        requires_geodata: true,
        geodata_present: false,
        online: false,
    };
    let outcome = v
        .preflight(&body("rules:\n  - GEOIP,CN,DIRECT\n"), &context)
        .await
        .expect("preflight");
    assert!(
        matches!(outcome, LevelOutcome::Skipped(_)),
        "an offline host must skip, not fail: {outcome:?}"
    );
}

/// An occupied port is a failure at preflight, because activating would leave
/// the kernel bound to nothing for that port.
#[tokio::test]
async fn preflight_fails_when_a_wanted_port_is_taken() {
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), false).await;

    // Hold a port for the duration of the check.
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", 0))
        .await
        .expect("bind");
    let taken = listener.local_addr().expect("addr").port();

    let context = PreflightContext::simple(vec![taken]);
    let outcome = v
        .preflight(&body("mode: rule\n"), &context)
        .await
        .expect("preflight");
    assert!(outcome.is_failed(), "a taken port must fail: {outcome:?}");
    assert!(outcome.summary().contains(&taken.to_string()));
}

#[tokio::test]
async fn preflight_passes_when_the_wanted_port_is_free() {
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), false).await;

    // Bind then release, so the port is almost certainly free.
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", 0))
        .await
        .expect("bind");
    let free = listener.local_addr().expect("addr").port();
    drop(listener);

    let context = PreflightContext::simple(vec![free]);
    let outcome = v
        .preflight(&body("mode: rule\n"), &context)
        .await
        .expect("preflight");
    assert!(outcome.is_passed(), "{outcome:?}");
}

#[tokio::test]
async fn observe_port_usage_reports_only_bound_ports() {
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), false).await;

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", 0))
        .await
        .expect("bind");
    let taken = listener.local_addr().expect("addr").port();
    let free_listener = tokio::net::TcpListener::bind(("0.0.0.0", 0))
        .await
        .expect("bind");
    let free = free_listener.local_addr().expect("addr").port();
    drop(free_listener);

    let in_use = v.observe_port_usage(&[taken, free]).await.expect("observe");
    assert!(
        in_use.contains(&taken),
        "the held port must be reported: {in_use:?}"
    );
    assert!(
        !in_use.contains(&free),
        "the free port must not be: {in_use:?}"
    );
}

// ------------------------------------------------- kernel-backed (needs mihomo)

/// A syntax-broken document must fail the semantic layer *without* invoking the
/// kernel, which would otherwise create state.
#[tokio::test]
async fn semantic_does_not_run_the_kernel_on_a_syntax_error() {
    let dir = tempfile::tempdir().expect("dir");
    // A missing binary proves the kernel was not reached.
    let v = validator(dir.path(), false).await;
    let outcome = v
        .validate_semantic(&body("a: [unclosed\n"))
        .await
        .expect("semantic");
    assert!(outcome.is_failed(), "{outcome:?}");
    assert!(
        outcome.summary().contains("invalid YAML"),
        "the syntax layer's reason must be what surfaces: {outcome:?}"
    );
}

#[tokio::test]
async fn semantic_reports_a_missing_kernel_binary_as_an_error() {
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), false).await;
    let outcome = v.validate_semantic(&body("mode: rule\n")).await;
    assert!(
        outcome.is_err(),
        "an unrunnable kernel is an error, not a finding"
    );
}

#[tokio::test]
async fn a_valid_config_passes_the_kernel_check() {
    let Some(_) = test_binary() else {
        return;
    };
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), true).await;

    let outcome = v
        .validate_semantic(&body(
            "mixed-port: 17890\nmode: rule\nlog-level: warning\n\
             proxies: []\nproxy-groups: []\nrules:\n  - MATCH,DIRECT\n",
        ))
        .await
        .expect("semantic");
    assert!(outcome.is_passed(), "{outcome:?}");
}

/// The measured behaviour this adapter exists for: the kernel accepts a
/// mistyped key, so only the whitelist catches it.
#[tokio::test]
async fn a_misspelled_key_is_caught_even_though_the_kernel_accepts_it() {
    let Some(_) = test_binary() else {
        return;
    };
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), true).await;

    let outcome = v
        .validate_semantic(&body(
            "mixed-portt: 17890\nmode: rule\nrules:\n  - MATCH,DIRECT\n",
        ))
        .await
        .expect("semantic");

    assert!(
        outcome.is_failed(),
        "the whitelist must catch what the kernel ignores: {outcome:?}"
    );
    assert!(
        outcome.summary().contains("mixed-portt"),
        "the offending key must be named: {outcome:?}"
    );
}

/// A type error is the kernel's job and must still be reported.
#[tokio::test]
async fn a_type_error_is_reported_by_the_kernel_check() {
    let Some(_) = test_binary() else {
        return;
    };
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), true).await;

    let outcome = v
        .validate_semantic(&body("mixed-port: not-a-number\nmode: rule\n"))
        .await
        .expect("semantic");
    assert!(outcome.is_failed(), "a bad type must fail: {outcome:?}");
}

/// The kernel must not leave anything behind: measured, a geo-referencing config
/// downloads 12.8 MB into its working directory. The scratch directory is
/// removed afterwards, so the parent must be left empty.
#[tokio::test]
async fn the_kernel_check_leaves_no_scratch_directory() {
    let Some(_) = test_binary() else {
        return;
    };
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), true).await;

    let _ = v
        .validate_semantic(&body(
            "mixed-port: 17890\nmode: rule\nrules:\n  - MATCH,DIRECT\n",
        ))
        .await
        .expect("semantic");

    let scratch = dir.path().join("scratch");
    if let Ok(mut entries) = tokio::fs::read_dir(&scratch).await {
        assert!(
            entries.next_entry().await.expect("entry").is_none(),
            "the scratch directory must be empty; the kernel's downloads leaked"
        );
    }
}

/// A geo-referencing config is where the kernel's side effects appear, so it is
/// the case that most needs the cleanup assertion.
#[tokio::test]
async fn a_geo_config_does_not_leak_downloads_into_the_scratch_root() {
    let Some(_) = test_binary() else {
        return;
    };
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), true).await;

    let _ = v
        .validate_semantic(&body(
            "mixed-port: 17891\nmode: rule\nrules:\n  - GEOIP,CN,DIRECT\n  - MATCH,DIRECT\n",
        ))
        .await
        .expect("semantic");

    let scratch = dir.path().join("scratch");
    if let Ok(mut entries) = tokio::fs::read_dir(&scratch).await {
        let leftover = entries.next_entry().await.expect("entry");
        assert!(
            leftover.is_none(),
            "the kernel's geo downloads must be removed with the scratch directory, \
             found {:?}",
            leftover.map(|e| e.path())
        );
    }
}

/// The candidate must be confirmed to exist, because the kernel reports success
/// after *creating* a missing file.
#[tokio::test]
async fn the_candidate_file_is_written_before_the_kernel_runs() {
    let Some(_) = test_binary() else {
        return;
    };
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), true).await;

    let outcome = v
        .validate_semantic(&body(
            "mixed-port: 17892\nmode: rule\nrules:\n  - MATCH,DIRECT\n",
        ))
        .await
        .expect("semantic");
    assert!(outcome.is_passed(), "{outcome:?}");
}

/// Existing geo data is copied in, so validating does not re-download it.
#[tokio::test]
async fn existing_geodata_is_reused_during_validation() {
    let Some(_) = test_binary() else {
        return;
    };
    let dir = tempfile::tempdir().expect("dir");
    let data = dir.path().join("data");
    tokio::fs::create_dir_all(&data).await.expect("data dir");
    // A placeholder is enough: the point is that the file is offered to the
    // kernel, not that its contents are valid.
    tokio::fs::write(data.join("GeoSite.dat"), b"placeholder")
        .await
        .expect("write");

    let v = KernelConfigValidator::new(
        test_binary().expect("binary"),
        data.clone(),
        dir.path().join("scratch"),
    );
    assert!(
        v.geodata_present().await,
        "the placeholder must be detected"
    );
}

#[tokio::test]
async fn geodata_absence_is_detected() {
    let dir = tempfile::tempdir().expect("dir");
    let v = validator(dir.path(), false).await;
    assert!(!v.geodata_present().await);
}
