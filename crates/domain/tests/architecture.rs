//! Architecture guards.
//!
//! Layering rules are easy to violate accidentally and hard to notice in review:
//! one `use tokio::...` in a domain file turns a pure model into one that needs
//! a runtime, and nothing in a normal test suite would fail.
//!
//! These tests read `Cargo.toml` and the source tree to assert the boundary, so
//! a violation fails `cargo test` rather than surfacing months later.

use std::path::{Path, PathBuf};

/// Walks up from the test binary's manifest directory to the crate root.
fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Every `.rs` file under `src/`, recursively.
fn source_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&crate_root().join("src"), &mut files);
    files
}

/// Crates that must never appear as domain dependencies.
///
/// Each would break a rule from `AGENTS.md`: runtimes and HTTP/DB clients make
/// the model untestable in isolation, and `async-trait` would introduce async
/// ports into a layer that must stay synchronous.
const FORBIDDEN_DEPENDENCIES: &[&str] = &[
    "tokio",
    "reqwest",
    "sqlx",
    "axum",
    "tower",
    "hyper",
    "async-trait",
    "serde",
    "serde_json",
    "clap",
    "ratatui",
    "crossterm",
];

#[test]
fn manifest_declares_no_forbidden_dependencies() {
    let manifest = std::fs::read_to_string(crate_root().join("Cargo.toml"))
        .expect("domain Cargo.toml must be readable");

    for forbidden in FORBIDDEN_DEPENDENCIES {
        // Match a dependency line starting with the crate name, so `serde_json`
        // is not matched by a search for `serde` alone and vice versa.
        let as_dependency = format!("\n{forbidden} ");
        let as_inline = format!("\n{forbidden}=");
        assert!(
            !manifest.contains(&as_dependency) && !manifest.contains(&as_inline),
            "domain must not depend on `{forbidden}`; it breaks the layering rules in AGENTS.md"
        );
    }
}

#[test]
fn sources_do_not_reference_forbidden_crates() {
    let mut violations = Vec::new();

    for file in source_files() {
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        for forbidden in FORBIDDEN_DEPENDENCIES {
            for pattern in [
                format!("use {forbidden}::"),
                format!("extern crate {forbidden}"),
                format!("{forbidden}::"),
            ] {
                if text.contains(&pattern) {
                    violations.push(format!(
                        "{} references `{forbidden}` (matched `{pattern}`)",
                        file.display()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "domain sources must not reference infrastructure crates:\n{}",
        violations.join("\n")
    );
}

/// The domain must not touch the filesystem, spawn processes, or open sockets.
///
/// `std::net::IpAddr` is permitted — it is a plain value type used for address
/// classification — but `TcpStream`/`UdpSocket` are not.
#[test]
fn sources_do_not_perform_io() {
    const FORBIDDEN_MODULES: &[&str] = &[
        "std::fs",
        "std::process",
        "std::net::TcpStream",
        "std::net::UdpSocket",
        "std::net::TcpListener",
        "std::os::unix::net",
    ];

    let mut violations = Vec::new();

    for file in source_files() {
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        for module in FORBIDDEN_MODULES {
            if text.contains(module) {
                violations.push(format!("{} references `{module}`", file.display()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "domain must perform no IO:\n{}",
        violations.join("\n")
    );
}

/// There must be no `unsafe` blocks; the crate already forbids them, but this
/// checks the source text so the rule holds even if the attribute is dropped.
#[test]
fn sources_contain_no_unsafe() {
    for file in source_files() {
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        assert!(
            !text.contains("unsafe "),
            "{} contains `unsafe`, which the domain forbids",
            file.display()
        );
    }
}

/// Domain must not depend on any sibling crate in the workspace.
#[test]
fn manifest_has_no_workspace_sibling_dependencies() {
    let manifest = std::fs::read_to_string(crate_root().join("Cargo.toml"))
        .expect("domain Cargo.toml must be readable");

    for sibling in [
        "proxy-application",
        "proxy-infrastructure",
        "proxy-interfaces",
        "proxy-bootstrap",
    ] {
        assert!(
            !manifest.contains(sibling),
            "domain must not depend on `{sibling}`; dependencies point inward only"
        );
    }
}
