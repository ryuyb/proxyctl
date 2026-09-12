//! Adoption of a kernel that outlived the agent.
//!
//! Ignored by default: it needs Linux, `/proc`, and permission to spawn and
//! signal processes. It exists because adoption is the one behaviour that cannot
//! be verified on the development host — macOS has no `/proc` — and because it is
//! the behaviour that prevents a duplicate kernel after an agent restart.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::time::Duration;

use proxy_application::ports::process_manager::{ProcessManager, ProcessStatus, StartOptions};
use proxy_infrastructure::process::SupervisedChildProcess;

/// A scratch directory for the fake kernel's working files.
fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("proxyctl-adopt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("scratch dir");
    path
}

/// The real kernel, from `PROXYCTL_TEST_BINARY`, or `None` to skip.
///
/// A stand-in cannot be used here. Two properties of a real kernel are what makes
/// the matcher work, and no unrelated binary has both:
///
/// * `/proc/<pid>/exe` must be the binary itself. For a shell script it is the
///   *interpreter* — `/usr/bin/dash` — so a script can never match.
/// * The process must accept a kernel-shaped command line and stay alive. `sleep`
///   rejects `-d` and exits immediately.
///
/// So these tests run against mihomo, and skip when it is not provided.
fn kernel_binary() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var("PROXYCTL_TEST_BINARY").ok()?);
    path.exists().then_some(path)
}

/// A minimal kernel configuration.
fn write_config(dir: &std::path::Path, port: u16) -> PathBuf {
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "mixed-port: {port}\nbind-address: 127.0.0.1\nsecret: \"s\"\nmode: rule\n\
             log-level: warning\nproxies: []\nrules:\n  - MATCH,DIRECT\n"
        ),
    )
    .expect("config");
    config
}

fn options(
    binary: &std::path::Path,
    dir: &std::path::Path,
    config: &std::path::Path,
) -> StartOptions {
    StartOptions {
        binary_path: binary.to_string_lossy().into_owned(),
        working_dir: dir.to_string_lossy().into_owned(),
        config_path: config.to_string_lossy().into_owned(),
        required_capabilities: Vec::new(),
    }
}

/// The scenario this whole design exists for: a kernel started by a previous
/// agent is rediscovered, not duplicated.
#[tokio::test]
#[ignore = "requires linux, /proc, and process signalling"]
async fn discovers_and_adopts_a_process_it_did_not_spawn() {
    let Some(binary) = kernel_binary() else {
        return;
    };
    let dir = scratch("discover");
    let config = write_config(&dir, 17890);

    // Spawn a kernel that this supervisor does not know about, standing in for
    // one started by a previous agent incarnation.
    // Invoked the way the supervisor invokes a kernel: the working directory
    // appears as an argument, which is what discovery matches on.
    // A kernel started the way the supervisor starts one, standing in for one
    // started by a previous agent incarnation.
    let mut orphan = tokio::process::Command::new(&binary)
        .arg("-d")
        .arg(&dir)
        .arg("-f")
        .arg(&config)
        .spawn()
        .expect("spawn orphan");
    let orphan_pid = orphan.id().expect("pid");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // A fresh supervisor must find it rather than concluding nothing runs.
    let supervisor = SupervisedChildProcess::new();
    let found = supervisor
        .discover(&options(&binary, &dir, &config))
        .await
        .expect("discovery runs");

    let handle = found.expect("the running kernel must be discovered");
    assert_eq!(
        handle.pid, orphan_pid,
        "it must find the process that exists"
    );
    assert!(
        supervisor.is_alive(&handle).await.expect("checked"),
        "the adopted handle must be verifiable"
    );
    assert_eq!(
        supervisor.status(&handle).await.expect("status"),
        ProcessStatus::Running
    );

    // And it must be manageable, which is the point of adopting it.
    let stopped = supervisor
        .stop(&handle, Duration::from_secs(5))
        .await
        .expect("an adopted process can be stopped");
    assert!(!stopped.forced, "a sleeping process stops on SIGTERM");

    let _ = orphan.kill().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Adoption must not reach for an unrelated process: the handle it returns is
/// used to send signals.
#[tokio::test]
#[ignore = "requires linux, /proc, and process signalling"]
async fn does_not_adopt_an_unrelated_process() {
    let Some(binary) = kernel_binary() else {
        return;
    };
    let dir = scratch("unrelated");
    let config = write_config(&dir, 17891);

    let supervisor = SupervisedChildProcess::new();
    // Nothing matching those options is running.
    let found = supervisor
        .discover(&options(&binary, &dir, &config))
        .await
        .expect("discovery runs");

    assert!(found.is_none(), "nothing should be adopted");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A recycled pid must not be mistaken for the kernel.
#[tokio::test]
#[ignore = "requires linux and /proc"]
async fn a_handle_with_the_wrong_start_time_is_not_alive() {
    let Some(binary) = kernel_binary() else {
        return;
    };
    let dir = scratch("stale");
    let config = write_config(&dir, 17892);

    let mut child = tokio::process::Command::new(&binary)
        .arg("-d")
        .arg(&dir)
        .arg("-f")
        .arg(&config)
        .spawn()
        .expect("spawn");
    let pid = child.id().expect("pid");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let supervisor = SupervisedChildProcess::new();
    let real = supervisor
        .discover(&options(&binary, &dir, &config))
        .await
        .expect("discovery")
        .expect("found");
    assert_eq!(real.pid, pid);

    // Same pid, different start time: what a recycled pid would look like.
    let stale = proxy_application::ports::process_manager::ProcessHandle::new(
        real.pid,
        real.start_time.wrapping_add(1),
    );
    assert!(
        !supervisor.is_alive(&stale).await.expect("checked"),
        "a stale handle must not read as alive"
    );

    let _ = child.kill().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Stopping an already-exited process is success, so recovery paths do not have
/// to distinguish the cases.
#[tokio::test]
#[ignore = "requires linux and /proc"]
async fn stopping_an_exited_process_succeeds() {
    let Some(binary) = kernel_binary() else {
        return;
    };
    let dir = scratch("exited");
    let config = write_config(&dir, 17893);

    let mut child = tokio::process::Command::new(&binary)
        .arg("-d")
        .arg(&dir)
        .arg("-f")
        .arg(&config)
        .spawn()
        .expect("spawn");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let supervisor = SupervisedChildProcess::new();
    let handle = supervisor
        .discover(&options(&binary, &dir, &config))
        .await
        .expect("discovery")
        .expect("found");

    let _ = child.kill().await;
    let _ = child.wait().await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let status = supervisor
        .stop(&handle, Duration::from_secs(1))
        .await
        .expect("stopping a gone process succeeds");
    assert!(!status.forced);

    let _ = std::fs::remove_dir_all(&dir);
}
