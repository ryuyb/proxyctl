//! Architecture guards for the client half of `proxyctl`.
//!
//! # Why this crate needs a guard more than the others
//!
//! `proxyctl` is one binary with two roles, and it links both halves. Nothing in
//! the type system stops the client modules from calling a use case directly —
//! the dependency is right there in `Cargo.toml`. What keeps the socket the only
//! path is therefore a convention, and a convention that only exists in a
//! document is one that quietly stops being true.
//!
//! So the rule is executable: `client` and `command` may speak HTTP and format
//! strings, and they may not name the application, domain, or bootstrap crates.
//! If that ever needs to change, the test fails and the change becomes a decision
//! rather than an accident.
//!
//! # Why it matters beyond tidiness
//!
//! Three properties depend on the client *not* being able to reach inward:
//!
//! - **The access-control boundary.** The socket's mode (`0660`) plus the peer
//!   check is what limits who can manage the kernel. A direct call has no peer
//!   and no mode.
//! - **Serialization.** Per-instance locks are in-process and the agent owns them.
//!   A client that called a use case would run under a *different* process's lock
//!   table and could interleave with the agent's own operation.
//! - **A single contract.** Every operation is exercised through the API, so the
//!   API's tests cover what the CLI actually does. A second entry point would be
//!   exercised by nothing.
//!
//! These tests read the source tree, so a violation fails the build.

use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// The modules that make up the client half.
const CLIENT_MODULES: &[&str] = &["src/client", "src/command", "src/exit.rs"];

/// Crates the client half must never name.
const FORBIDDEN: &[&str] = &[
    "proxy_application",
    "proxy_domain",
    "proxy_bootstrap",
    // The server half of this very crate: a client module reaching into `agent`
    // would let `proxyctl status` compose a context, which is the direct-call path
    // wearing a different name.
    "crate::agent",
];

/// Reads every `.rs` file under `dir`, recursively.
fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            files_under(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The client half's sources, as `(path, text)`.
fn client_sources() -> Vec<(PathBuf, String)> {
    let root = crate_root();
    let mut paths = Vec::new();
    for module in CLIENT_MODULES {
        let path = root.join(module);
        if path.is_dir() {
            files_under(&path, &mut paths);
        } else if path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    assert!(
        !paths.is_empty(),
        "the guard must find the client sources; a moved module would silently disable it"
    );
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).expect("client source must be readable");
            (path, text)
        })
        .collect()
}

/// Strips comments, so a guard does not fire on prose that mentions a name.
///
/// The module docs in this crate discuss `proxy_application` by name, and a naive
/// scan would treat that explanation as a violation — which would push the
/// explanation out of the code, exactly backwards.
fn code_only(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let bytes: Vec<char> = source.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        // A line comment runs to the end of the line. `//` inside a string literal
        // would be misread, but the strings in this crate are paths and messages,
        // and a false *negative* here is caught by the tests that read the tree.
        if bytes[i] == '/' && i + 1 < bytes.len() && bytes[i + 1] == '/' {
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// The client half must not name anything below the transport.
#[test]
fn the_client_half_does_not_reach_into_the_agent_half() {
    for (path, text) in client_sources() {
        let code = code_only(&text);
        for forbidden in FORBIDDEN {
            assert!(
                !code.contains(forbidden),
                "{} names `{forbidden}`; the client must reach the agent only over the socket",
                path.display()
            );
        }
    }
}

/// The client half must not import the crates that implement the agent, even
/// indirectly through a re-export.
#[test]
fn the_client_half_imports_nothing_from_the_agent_half() {
    for (path, text) in client_sources() {
        for line in code_only(&text).lines() {
            let line = line.trim();
            if !(line.starts_with("use ") || line.starts_with("pub use ")) {
                continue;
            }
            for forbidden in ["proxy_application", "proxy_domain", "proxy_bootstrap"] {
                assert!(
                    !line.contains(forbidden),
                    "{} has `{line}`; the client must not import {forbidden}",
                    path.display()
                );
            }
        }
    }
}

/// `client/` is the transport. It must not know about commands, the grammar, or
/// the daemon: a transport that knew what it was carrying could not be reused,
/// and the layering would be gone.
#[test]
fn the_transport_does_not_depend_on_the_layers_above_it() {
    let root = crate_root();
    let mut paths = Vec::new();
    files_under(&root.join("src/client"), &mut paths);
    paths.sort();
    assert!(!paths.is_empty(), "the transport sources must exist");

    for path in paths {
        let text = std::fs::read_to_string(&path).expect("readable");
        let code = code_only(&text);
        for forbidden in [
            "crate::command",
            "crate::args",
            "crate::dispatch",
            "crate::agent",
        ] {
            assert!(
                !code.contains(forbidden),
                "{} names `{forbidden}`; the transport must sit below the layers that use it",
                path.display()
            );
        }
    }
}

/// The daemon half is allowed to be deep; it must not have become the client's
/// dependency, which would mean the `agent` module leaked into the command path.
#[test]
fn the_entry_point_is_the_only_place_both_halves_meet() {
    let root = crate_root();
    let main = std::fs::read_to_string(root.join("src/main.rs")).expect("main.rs must exist");
    // The binary mentions both, and that is correct — it is the seam.
    assert!(main.contains("dispatch"), "main.rs must dispatch");
    assert!(main.contains("Cli"), "main.rs must parse the grammar");

    // And neither client module may be imported *by* the agent module.
    let agent = std::fs::read_to_string(root.join("src/agent.rs")).expect("agent.rs must exist");
    let code = code_only(&agent);
    for forbidden in ["crate::command", "crate::dispatch"] {
        assert!(
            !code.contains(forbidden),
            "agent.rs names `{forbidden}`; the daemon must not depend on the client's shape"
        );
    }
}

/// The prefix is duplicated in `command` because the client must not link the
/// server half. That duplication is only safe while the two values agree, so the
/// server's own constant is read from its source and compared.
///
/// A drift here would 404 every command at once, and the symptom would look like a
/// server problem rather than a version skew.
#[test]
fn the_client_prefix_equals_the_servers_prefix() {
    let server = crate_root().join("../interfaces/src/http/routes/mod.rs");
    let text = std::fs::read_to_string(&server).expect("the server route table must be readable");
    let expected = text
        .lines()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix("pub const API_PREFIX: &str = ")
                .and_then(|rest| rest.strip_suffix(';'))
        })
        .map(|value| value.trim().trim_matches('"').to_owned())
        .expect("the server must declare API_PREFIX");

    // The prefix is defined in `endpoint`, which sits below `command` so a
    // transport can name it without reaching upward. This follows it there.
    let client = std::fs::read_to_string(crate_root().join("src/endpoint.rs"))
        .expect("the endpoint module must be readable");
    let actual = client
        .lines()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix("pub const API_PREFIX: &str = ")
                .and_then(|rest| rest.strip_suffix(';'))
        })
        .map(|value| value.trim().trim_matches('"').to_owned())
        .expect("the client must declare API_PREFIX");

    assert_eq!(
        actual, expected,
        "the client and server prefix must agree, or every command 404s"
    );
}

/// Every path the client builds must match a route the server registers.
///
/// This is the guard that catches a renamed endpoint. It reads both sources, so it
/// fails in the same commit as the rename rather than in the next manual test.
///
/// # Matching the two forms
///
/// The client writes paths as `format!("{API_PREFIX}/configs/{}/activate", id)` and
/// the server registers them as `format!("{API_PREFIX}/configs/{{id}}/activate")`.
/// Neither string is a route on its own, so both are reduced to the same shape:
/// take the literal prefix, then replace each interpolation — `{}` on the client's
/// side, `{{id}}` on the server's — with a single placeholder.
#[test]
fn every_client_path_has_a_registered_route() {
    let registered = route_shapes(&read("../interfaces/src/http/routes/mod.rs"));
    assert!(
        registered.len() >= 15,
        "the route table should have many routes; found {registered:?}"
    );

    let built = route_shapes(&read("src/command/mod.rs"));
    assert!(
        !built.is_empty(),
        "the client must build at least one path; a parse failure would make this test vacuous"
    );

    for path in &built {
        assert!(
            registered.contains(path),
            "the client builds `{path}`, which the server does not register; \
             registered routes are {registered:?}"
        );
    }
}

/// Reads a source file relative to the crate root.
fn read(relative: &str) -> String {
    let path = crate_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

/// Replaces each `{...}` interpolation with a single `{}` placeholder.
///
/// Hand-rolled rather than regex-based so the test needs no extra dependency, and
/// so the behaviour on nested braces is explicit.
fn normalize_interpolations(shape: &str) -> String {
    let mut out = String::with_capacity(shape.len());
    let mut depth = 0usize;
    for ch in shape.chars() {
        match ch {
            '{' => {
                depth += 1;
                if depth == 1 {
                    out.push_str("{}");
                }
            }
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Extracts the route shapes a source file declares or builds.
///
/// Returns them sorted and de-duplicated so a comparison reads as a set.
fn route_shapes(source: &str) -> Vec<String> {
    let mut shapes = Vec::new();
    for line in source.lines() {
        let Some(start) = line.find("{API_PREFIX}") else {
            continue;
        };
        let rest = &line[start + "{API_PREFIX}".len()..];
        // The path ends at the closing quote of the literal.
        let Some(end) = rest.find('"') else { continue };
        let literal = &rest[..end];
        // Only a real path segment counts: a line that merely *mentions* the
        // constant in prose or in a test would otherwise contribute an empty shape
        // and fail the comparison for the wrong reason.
        if !literal.starts_with('/') {
            continue;
        }

        // Reduce both conventions to one placeholder. The order matters: the
        // server's double-braced form is collapsed first, so its inner braces are
        // not mistaken for the client's empty interpolation.
        let mut shape = literal.replace("{{", "{").replace("}}", "}");
        // A named interpolation in either convention becomes the same placeholder:
        // the client writes `{segment}`, the server writes `{{id}}`, and both mean
        // "one path parameter".
        shape = normalize_interpolations(&shape);
        // A query string is not part of the route.
        let shape = shape.split('?').next().unwrap_or(&shape).to_owned();
        // A trailing interpolation used as an identifier is a route parameter.
        shapes.push(shape);
    }
    shapes.sort();
    shapes.dedup();
    shapes
}

/// The guard must actually read code, not just find files. If `client_sources`
/// ever returned nothing, every assertion above would pass vacuously.
#[test]
fn the_guard_reads_a_non_trivial_amount_of_client_code() {
    let sources = client_sources();
    let total: usize = sources.iter().map(|(_, text)| text.len()).sum();
    assert!(
        total > 4_000,
        "the client half should be more than a stub; read {total} bytes from {} files",
        sources.len()
    );
}

/// A comment mentioning a forbidden name must not trip the guard, or the module
/// documentation that explains the rule would have to be deleted to satisfy it.
#[test]
fn the_comment_stripper_removes_prose_but_keeps_code() {
    let stripped = code_only("// proxy_application is forbidden\nlet x = 1; // proxy_domain");
    assert!(!stripped.contains("proxy_application"));
    assert!(!stripped.contains("proxy_domain"));
    assert!(stripped.contains("let x = 1;"));
}
