//! Guards on the commands the documentation tells people to run.
//!
//! # Why this exists
//!
//! `README.md` and the installer's closing advice both recommended
//! `proxyctl mihomo install`. That subcommand has never existed — it is
//! `mihomo update`, and it additionally *requires* a version argument. The
//! documentation was written from memory, and the failure surfaced the first
//! time a reader copied a command. It was the second defect of exactly this
//! shape: `config validate` had been documented with the wrong argument form
//! earlier in the same document.
//!
//! A README is not checked by any compiler, so the only defence is to check what
//! it says against the program. These tests parse the documented commands out of
//! the files and ask clap whether each one exists.
//!
//! # What is deliberately not checked
//!
//! Argument *values*. The tests confirm `mihomo update` accepts a version
//! argument; they cannot know that `v1.19.30` is a real release. That is a
//! property of the upstream project, not of this one, and asserting it here
//! would make the suite fail when upstream removes an old tag.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspace root, from this crate's manifest directory.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root should resolve")
}

/// The binary under test.
///
/// `CARGO_BIN_EXE_` is set by Cargo for a crate with a binary target, so this is
/// the binary that was just built rather than whatever is on `PATH` — which is
/// what makes the test meaningful on a machine with an older release installed.
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_proxyctl"))
}

/// Whether the binary recognises a command path.
///
/// `--help` is asked for rather than the command being run, because running it
/// would need an agent. That works for recognising *subcommands* — clap resolves
/// the path before printing — but it does **not** validate required arguments,
/// so this says nothing about whether a particular invocation would succeed.
/// [`rejects_without`] covers that.
fn recognises(args: &[&str]) -> bool {
    Command::new(binary())
        .args(args)
        .arg("--help")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Whether the binary refuses the arguments on its own terms.
///
/// Exit code `2` is what clap uses for a usage error, which is what a missing
/// required argument produces. Any other outcome — including `1`, which the CLI
/// uses for a runtime failure — means the arguments were accepted and the
/// command tried to do something, which is the distinction this needs.
fn rejects_without(args: &[&str]) -> bool {
    Command::new(binary())
        .args(args)
        .output()
        .map(|out| out.status.code() == Some(2))
        .unwrap_or(false)
}

/// The binary's own top-level commands.
///
/// Listed rather than discovered, because the point of parsing prose is to find
/// commands *as a reader would copy them*, and a reader copies a word they have
/// seen. This is the vocabulary that makes the parse unambiguous: prose like
/// "proxyctl and the web interface" has no such word after `proxyctl`.
const TOP_LEVEL: &[&str] = &[
    "agent",
    "audit",
    "config",
    "connections",
    "doctor",
    "events",
    "jobs",
    "logs",
    "mihomo",
    "reload",
    "restart",
    "start",
    "status",
    "stop",
    "subscription",
    "system",
    "token",
    "tui",
];

/// Every `proxyctl …` invocation in a document.
///
/// Only command words are taken. An argument that is a placeholder (`<VERSION>`),
/// a value (`v1.19.30`), or an option (`--json`) stops the parse, because this
/// checks that a *command path* exists rather than that a particular invocation
/// would succeed.
///
/// The line is required to be a plausible command line rather than prose: the
/// token after `proxyctl` must be a known top-level command, and it must be
/// preceded by nothing but whitespace, a shell prompt, or a word such as `sudo`
/// or `-u <user>`. Without that, a sentence mentioning the program parses as an
/// invocation, which is what the first version of this test did.
fn documented_commands(text: &str) -> Vec<Vec<String>> {
    let mut found = Vec::new();

    for line in text.lines() {
        // Strip an inline `#` comment, which is how the documents annotate a
        // command, and any fenced-code marker.
        let line = line.split(" # ").next().unwrap_or(line);
        let line = line.trim_start_matches(['|', ' ', '\t']);

        let Some(start) = line.find("proxyctl ") else {
            continue;
        };

        // Whatever precedes `proxyctl` must be something that introduces a
        // command rather than a sentence.
        let before = line[..start].trim();
        let plausible = before.is_empty()
            || before.ends_with('$')
            || before.ends_with('|')
            || before == "sudo"
            || before.starts_with("sudo ");
        if !plausible {
            continue;
        }

        let rest = &line[start + "proxyctl ".len()..];
        let mut words: Vec<String> = Vec::new();
        for word in rest.split_whitespace() {
            let word = word.trim_end_matches(['.', ',', '`', ')', ':']);
            let is_command_word = !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-' || c == '_');
            if !is_command_word {
                break;
            }
            words.push(word.to_owned());
        }

        // The first word has to be a command this program actually has; that is
        // what separates `proxyctl status` from `proxyctl and its systemd unit`.
        match words.first() {
            Some(first) if TOP_LEVEL.contains(&first.as_str()) => found.push(words),
            _ => continue,
        }
    }

    found
}

/// The documentation files whose commands must exist, and which are committed.
fn documents() -> Vec<(PathBuf, String)> {
    ["README.md", "README.zh-CN.md", "scripts/install.sh"]
        .iter()
        .filter_map(|name| {
            let path = root().join(name);
            std::fs::read_to_string(&path).ok().map(|text| (path, text))
        })
        .collect()
}

#[test]
fn the_documents_are_present_to_check() {
    let docs = documents();
    assert_eq!(
        docs.len(),
        3,
        "expected all three documents; found {:?}",
        docs.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
}

#[test]
fn the_extractor_finds_the_commands_that_are_there() {
    // A parser that returns nothing would make every other test vacuous, so it is
    // checked against a string whose answer is known.
    let text = "sudo -u proxy-agent proxyctl mihomo update v1.19.30\n\
                proxyctl status\n\
                proxyctl config validate FILE   # a comment\n";
    let found = documented_commands(text);
    assert_eq!(
        found,
        vec![
            vec!["mihomo".to_owned(), "update".to_owned()],
            vec!["status".to_owned()],
            vec!["config".to_owned(), "validate".to_owned()],
        ],
        "{found:?}"
    );
}

/// Every command in every document must be accepted by the binary.
#[test]
fn every_documented_command_exists() {
    let mut unknown: Vec<String> = Vec::new();

    for (path, text) in documents() {
        for words in documented_commands(&text) {
            let args: Vec<&str> = words.iter().map(String::as_str).collect();
            if !recognises(&args) {
                unknown.push(format!(
                    "{}: proxyctl {}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    words.join(" ")
                ));
            }
        }
    }

    assert!(
        unknown.is_empty(),
        "these documented commands do not exist:\n  {}",
        unknown.join("\n  ")
    );
}

/// `mihomo update` requires a version, and a *command line* in the documents must
/// show it.
///
/// Separate from the check above because a command can exist and still be
/// documented incompletely: `proxyctl mihomo update` with no argument parses to a
/// clap usage error, so a reader copying it gets "the following required
/// arguments were not provided" rather than an install.
///
/// Prose is exempt. The install script's header explains *why* the kernel is not
/// installed for you, and naming the command there without a version is a
/// sentence about it, not something anyone runs — flagging that would push the
/// explanation out of the file to satisfy a linter.
#[test]
fn the_kernel_install_is_documented_with_its_required_version() {
    for (path, text) in documents() {
        for line in text.lines() {
            if !line.contains("proxyctl mihomo update") {
                continue;
            }

            // A fenced-code line, a shell prompt, or an indented command is what a
            // reader copies. A comment is not.
            let stripped = line.trim_start();
            let is_a_command_line = !stripped.starts_with('#')
                && !stripped.starts_with("//")
                && (line.starts_with("    ") || line.contains("sudo ") || line.contains('$'));

            if !is_a_command_line {
                continue;
            }

            let names_a_version = line.contains("<version>")
                || line.contains("<VERSION>")
                || line.contains("<版本号>")
                || line.contains("v1.");
            assert!(
                names_a_version,
                "{}: `mihomo update` needs a version, but this line omits it:\n  {}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                line.trim()
            );
        }
    }
}

/// A bare `proxyctl mihomo update` really is rejected.
///
/// The test above depends on this being true; asserting it here means the
/// documentation check cannot silently become vacuous if the argument is ever
/// made optional.
#[test]
fn the_kernel_install_rejects_a_missing_version() {
    assert!(
        rejects_without(&["mihomo", "update"]),
        "`mihomo update` accepted no version where one is required; the \
         documentation rule for it should be revisited"
    );
}
