//! The shipped example configuration must stay valid.
//!
//! # Why this is a test and not a review note
//!
//! `packaging/config.toml.example` is the only documentation an operator reads
//! before writing the file that decides whether the agent starts at all. It is
//! also the file nothing executes, so it drifts: rename a field in the schema and
//! the example keeps teaching the old name, and the operator learns about it from
//! a refused start on a production host.
//!
//! These tests read the file that is actually shipped.

use std::path::{Path, PathBuf};

fn example_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/config.toml.example")
}

fn read_example() -> String {
    let path = example_path();
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

/// The file is valid as shipped, which for a fully-commented file means it parses
/// as an empty configuration. Every value then comes from the defaults.
#[test]
fn the_shipped_example_parses() {
    let text = read_example();
    let parsed = proxy_bootstrap::FileConfig::parse(&text).expect("the example must parse");
    assert_eq!(
        parsed,
        proxy_bootstrap::FileConfig::default(),
        "the example is documentation; uncommenting nothing must mean nothing is set"
    );
}

/// The scaffold block is the part an operator is meant to *copy*, so it must be
/// valid TOML once uncommented. This is the check that catches a scaffold that
/// names a field the schema does not have — the mistake that would otherwise
/// surface as a refused start on the operator's machine.
#[test]
fn the_scaffold_is_valid_once_uncommented() {
    let text = read_example();

    // The scaffold starts at its first commented section header and ends at the
    // reference's first separator. Anchoring on the header rather than on the
    // "SCAFFOLD" word keeps the banner prose out of the extracted block — it is
    // not TOML and would fail to parse for the wrong reason.
    let start = text
        .find("# [agent]")
        .expect("the example must keep its scaffold, or copying a whole section stops working");
    let after = &text[start..];
    let end = after
        .find("# ---------------------------------------------------------------------------")
        .expect("the scaffold must be followed by the reference section");
    let block = &after[..end];

    // Uncomment: drop a leading `#` and the single space that follows it. The
    // double-comment lines (`# # Omit...`) become real comments, which is what
    // they are.
    let uncommented: String = block
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            match trimmed.strip_prefix("# ") {
                Some(rest) => rest,
                None => trimmed.strip_prefix('#').unwrap_or(trimmed),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let parsed = proxy_bootstrap::FileConfig::parse(&uncommented)
        .unwrap_or_else(|e| panic!("the scaffold is not valid TOML once uncommented: {e}"));
    assert!(
        parsed.agent.is_some(),
        "the scaffold should configure at least the agent section; got {parsed:?}"
    );
    assert!(
        parsed.kernel.is_some(),
        "the scaffold should show the kernel section, which is where the secret lives"
    );
}

/// Every section the schema knows must appear in the reference, or an operator
/// reading the file will not learn it exists.
#[test]
fn the_reference_documents_every_section() {
    let text = read_example();
    for section in [
        "[agent]",
        "[paths]",
        "[kernel]",
        "[controller]",
        "[converter]",
        "[security]",
    ] {
        assert!(
            text.contains(&format!("# {section}")),
            "the example does not document {section}"
        );
    }
}

/// The example must state the mode it expects, because "the loader does not care"
/// is only true if the operator has been told so — otherwise the reasonable
/// assumption is the strict one, and they will `sudo` to edit a file that does not
/// need it.
#[test]
fn the_example_states_the_permission_expectation() {
    let text = read_example();
    assert!(
        text.contains("0666"),
        "the example must state the packaged mode, or an operator will assume a \
         stricter one and reach for sudo unnecessarily"
    );
    assert!(
        !text.contains("refuses to start otherwise"),
        "the example still claims a refusal the loader no longer performs"
    );
}
