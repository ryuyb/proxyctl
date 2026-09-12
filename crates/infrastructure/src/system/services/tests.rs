//! Tests for the systemd service manager.

use super::*;
use proxy_application::ports::service_manager::ServiceManager;

/// `degraded` is the case that matters: systemd is up with a failed unit, and
/// treating it as "no init system" would disable service management on a working
/// host. Real-machine measurement found a container reporting it from unrelated
/// units.
#[test]
fn degraded_is_still_systemd() {
    assert_eq!(classify_system_state("degraded"), InitSystem::Systemd);
    assert_eq!(classify_system_state("running"), InitSystem::Systemd);
    assert_eq!(classify_system_state("starting"), InitSystem::Systemd);
    assert_eq!(classify_system_state("maintenance"), InitSystem::Systemd);
    assert_eq!(classify_system_state("stopping"), InitSystem::Systemd);
}

#[test]
fn a_finished_or_absent_manager_is_not_systemd() {
    assert_eq!(classify_system_state("offline"), InitSystem::None);
    assert_eq!(classify_system_state("unknown"), InitSystem::None);
}

#[test]
fn an_unrecognized_state_is_unknown_not_none() {
    assert_eq!(classify_system_state("nonsense"), InitSystem::Unknown);
    assert_eq!(classify_system_state(""), InitSystem::Unknown);
}

#[test]
fn classification_tolerates_surrounding_whitespace() {
    // `systemctl` output is trimmed, but a caller may pass it raw.
    assert_eq!(classify_system_state("  running\n"), InitSystem::Systemd);
}

#[test]
fn the_default_unit_is_the_documented_one() {
    let manager = SystemdServiceManager::new();
    assert_eq!(manager.unit(), AGENT_UNIT);
    assert_eq!(AGENT_UNIT, "proxy-agent.service");
}

/// A missing binary must degrade to "no unit control" rather than erroring: the
/// port documents that as the normal container case.
#[tokio::test]
async fn a_missing_binary_reports_no_control() {
    let manager = SystemdServiceManager::with_paths("/nonexistent/systemctl", "x.service");

    assert!(!manager.supports_unit_control().await.expect("control"));
    assert!(!manager.is_agent_service_active().await.expect("active"));
}

/// With no `systemctl` and no manager directory, the answer is None rather than
/// Unknown: nothing suggests an init system exists.
#[tokio::test]
async fn detect_falls_back_to_the_filesystem() {
    let manager = SystemdServiceManager::with_paths("/nonexistent/systemctl", "x.service");
    let detected = manager.detect().await.expect("detect");

    // On a host with /run/systemd/system this is Unknown (present but unqueried);
    // without it, None. Either is honest; Systemd would not be.
    assert_ne!(
        detected,
        InitSystem::Systemd,
        "a missing binary cannot prove systemd is running"
    );
}

/// A real `systemctl`, when present, must answer consistently.
#[tokio::test]
async fn the_real_systemctl_is_queried_when_available() {
    if tokio::fs::metadata("/usr/bin/systemctl").await.is_err()
        && tokio::fs::metadata("/bin/systemctl").await.is_err()
    {
        return;
    }
    let manager = SystemdServiceManager::new();
    let detected = manager.detect().await.expect("detect");
    let control = manager.supports_unit_control().await.expect("control");

    // The two answers must not contradict each other: claiming systemd while
    // reporting no unit control would mean the manager is unreachable.
    if detected == InitSystem::Systemd {
        assert!(
            control,
            "systemd was detected but unit control is unavailable"
        );
    }
}
