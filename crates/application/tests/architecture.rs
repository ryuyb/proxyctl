//! Architecture guards for the application layer.
//!
//! The layering rules are easy to violate by accident and hard to catch in
//! review: a single `use reqwest::...` would make the use cases depend on one
//! HTTP client and quietly end the "adapters are replaceable" property this
//! layer exists to provide.
//!
//! These tests read `Cargo.toml` and the source tree, so a violation fails the
//! build rather than surfacing a year later.

use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

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

/// Crates that must never appear as application dependencies.
///
/// The application layer orchestrates; it must not know how any effect is
/// performed. Each entry here would couple the use cases to a specific adapter.
const FORBIDDEN_DEPENDENCIES: &[&str] = &[
    "reqwest",
    "hyper",
    "sqlx",
    "axum",
    "tower",
    "tower-http",
    "ratatui",
    "crossterm",
    "clap",
    "serde",
    "serde_json",
    "serde_yaml",
    "nix",
    "libc",
    "systemd",
];

/// Modules that indicate direct system access rather than going through a port.
const FORBIDDEN_MODULES: &[&str] = &[
    "std::process",
    "std::fs",
    "std::os::unix::net",
    "std::net::TcpStream",
    "std::net::UdpSocket",
    "std::net::TcpListener",
];

#[test]
fn manifest_declares_no_adapter_dependencies() {
    let manifest = std::fs::read_to_string(crate_root().join("Cargo.toml"))
        .expect("manifest must be readable");

    for forbidden in FORBIDDEN_DEPENDENCIES {
        let as_dependency = format!("\n{forbidden} ");
        let as_inline = format!("\n{forbidden}=");
        let as_table = format!("\n{forbidden}.");
        assert!(
            !manifest.contains(&as_dependency)
                && !manifest.contains(&as_inline)
                && !manifest.contains(&as_table),
            "application must not depend on `{forbidden}`; effects belong behind ports"
        );
    }
}

#[test]
fn manifest_depends_only_on_domain_and_async_primitives() {
    let manifest = std::fs::read_to_string(crate_root().join("Cargo.toml"))
        .expect("manifest must be readable");

    // A workspace sibling other than `proxy-domain` would invert the dependency
    // direction.
    for sibling in [
        "proxy-infrastructure",
        "proxy-interfaces",
        "proxy-bootstrap",
    ] {
        assert!(
            !manifest.contains(sibling),
            "application must not depend on `{sibling}`; dependencies point inward"
        );
    }
    assert!(
        manifest.contains("proxy-domain"),
        "application is expected to build on the domain"
    );
}

#[test]
fn sources_do_not_reference_forbidden_crates() {
    let mut violations = Vec::new();

    for file in source_files() {
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        // Only inspect code. Doc comments legitimately name these crates when
        // explaining why their types are kept out of the public surface, and
        // flagging prose would push authors toward writing less useful docs.
        let code = strip_doc_comments(&text);

        for forbidden in FORBIDDEN_DEPENDENCIES {
            for pattern in [format!("use {forbidden}::"), format!("{forbidden}::")] {
                if code.contains(&pattern) {
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
        "application sources must not reference adapter crates:\n{}",
        violations.join("\n")
    );
}

/// Removes `//!` and `///` comment lines, leaving code behind.
fn strip_doc_comments(source: &str) -> String {
    source
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !(trimmed.starts_with("//!") || trimmed.starts_with("///") || trimmed.starts_with("//"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// No direct process or filesystem access: every effect crosses a port.
#[test]
fn sources_do_not_perform_io_directly() {
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
        "application must reach effects through ports, not directly:\n{}",
        violations.join("\n")
    );
}

#[test]
fn sources_contain_no_unsafe() {
    for file in source_files() {
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        assert!(
            !text.contains("unsafe "),
            "{} contains `unsafe`, which this crate forbids",
            file.display()
        );
    }
}

/// Port traits must remain usable behind a trait object, since the bootstrap
/// layer assembles `Arc<dyn Port>` values at runtime.
#[test]
fn ports_use_async_trait_for_object_safety() {
    let ports_dir = crate_root().join("src").join("ports");
    let mut trait_files = Vec::new();

    let entries = std::fs::read_dir(&ports_dir).expect("ports dir readable");
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|ext| ext == "rs")
            && path.file_name() != Some("mod.rs".as_ref())
        {
            let text = std::fs::read_to_string(&path).expect("readable");
            if text.contains("pub trait ") {
                trait_files.push((path, text));
            }
        }
    }

    assert!(!trait_files.is_empty(), "expected to find port traits");

    for (path, text) in trait_files {
        // Every trait containing an async fn must carry the attribute; native
        // async-in-trait is not dyn-compatible.
        if text.contains("async fn") && text.contains("pub trait ") {
            assert!(
                text.contains("#[async_trait]") || text.contains("#[async_trait::async_trait]"),
                "{} declares an async trait without `#[async_trait]`, which breaks \
                 `Arc<dyn Port>` assembly",
                path.display()
            );
        }
    }
}
