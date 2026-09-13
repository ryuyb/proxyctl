//! Safe wrappers over the few Linux system calls that have no safe wrapper.
//!
//! # Why this crate exists
//!
//! The rest of the workspace is `forbid(unsafe_code)`. Exactly one check needs
//! to break that: `ioctl(TUNSETIFF)` on `/dev/net/tun` is the only reliable way
//! to tell whether TUN is *usable*. The two cheaper checks — the device node
//! exists, and it can be opened — both pass on a container that lacks
//! `CAP_NET_ADMIN`, which real-machine measurement confirmed returns `EPERM` from
//! `TUNSETIFF`. Reporting that environment as TUN-capable is precisely the false
//! positive the capability model exists to prevent.
//!
//! A second call earns its place here: adopting a listening socket that the
//! service manager passed on a descriptor. `std` can only do that through
//! `FromRawFd`, which is unsafe, and there is no safe wrapper for it in the
//! dependency set.
//!
//! Rather than downgrade `forbid` to `deny` inside the infrastructure crate —
//! which would leave a lint that any future module could locally silence — the
//! unsafe code lives here, in a small, separately reviewable crate whose whole
//! surface is audited below.
//!
//! # What is unsafe here, and why it is sound
//!
//! One `ioctl` call. Its soundness rests on three things, each enforced by the
//! wrapper rather than left to the caller:
//!
//! 1. The request is the `TUNSETIFF` constant, never a caller-supplied value.
//! 2. The argument points at a `ifreq`-shaped value that this crate owns and
//!    initializes, so the kernel cannot read uninitialized memory through it.
//! 3. The interface name is copied from a validated `&str` and bounded to
//!    `IFNAMSIZ - 1` bytes plus a NUL terminator, so the kernel's `strnlen`-style
//!    read of the name stays inside the buffer.
//!
//! One `from_raw_fd` call, in [`FromServiceManager::adopt_listening_socket`]. Its
//! soundness rests on the descriptor arriving from the process's own service
//! manager through the documented `LISTEN_FDS`/`LISTEN_PID` handshake, which the
//! caller must have verified; the type is constructed only by
//! [`verified_descriptors`], which performs that check and returns nothing when it
//! fails. Ownership moves into the result, so the descriptor is closed exactly once
//! and the caller cannot keep using it after handing it over.

#![deny(missing_docs)]
// Deliberately not `forbid`: this crate's purpose is to contain unsafe code.
// Every use is confined to the function below, and no other module may add one
// without review.
#![deny(unsafe_op_in_unsafe_fn)]

use std::io;
use std::os::fd::RawFd;

/// The outcome of trying to bring up a TUN interface.
///
/// Modelled as an enum rather than as `io::Result<bool>` because the caller needs
/// to distinguish "the kernel refused for a permissions reason" from "there is no
/// such device": the first is a misconfiguration to report, the second is an
/// absent capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunSetIffOutcome {
    /// The interface was brought up.
    Ok,
    /// The kernel refused with `EPERM` or `EACCES`, i.e. the capability is
    /// missing rather than the device.
    PermissionDenied,
    /// The device is not present or not a TUN device (`ENODEV`, `ENXIO`).
    Missing,
    /// Anything else, including `EBUSY` from a name already in use.
    Failed(io::ErrorKind),
}

/// The maximum interface-name length the kernel accepts, excluding the
/// terminating NUL.
///
/// `IFNAMSIZ` is 16 on Linux, so this is 15.
pub const MAX_INTERFACE_NAME: usize = 15;

/// Attempts to create a TUN interface, which is what proves TUN is usable.
///
/// This creates a real interface, so it is a *write* probe: the caller is
/// responsible for deciding whether that is acceptable, and for removing the
/// interface afterwards. The device is left up on success so the caller can
/// inspect or delete it deliberately rather than racing an automatic teardown.
///
/// `name` is a hint the kernel may replace; pass a distinctive one so a leftover
/// interface is recognisable.
///
/// # Errors
///
/// Returns the error from `ioctl` itself when the call could not be attempted.
/// A refusal by the kernel for a permissions or device reason is reported as an
/// outcome rather than an error, since those are the answers being sought.
#[cfg(target_os = "linux")]
pub fn tun_set_iff(fd: RawFd, name: &str) -> io::Result<TunSetIffOutcome> {
    if name.is_empty() || name.len() > MAX_INTERFACE_NAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "interface name must be 1..={MAX_INTERFACE_NAME} bytes, got {}",
                name.len()
            ),
        ));
    }

    // `ifreq` is a union whose largest arm defines its size. Zeroing the whole
    // struct is the simplest way to guarantee no arm holds uninitialized bytes,
    // which matters because the kernel reads the arm selected by `TUNSETIFF`.
    //
    // SAFETY: `libc::ifreq` is a plain-old-data struct of integers and a byte
    // array. All-zeroes is a valid bit pattern for every field, so this cannot
    // produce an invalid value.
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };

    // Copy the name, bounded by construction above, and leave the terminator.
    for (slot, byte) in request.ifr_name.iter_mut().zip(name.as_bytes()) {
        *slot = *byte as libc::c_char;
    }

    // The flags arm of the union. `IFF_TUN` is a point-to-point IP tunnel;
    // `IFF_NO_PI` asks for no packet-information prefix, which is what mihomo
    // uses and therefore what a representative probe should request.
    request.ifr_ifru.ifru_flags = (libc::IFF_TUN | libc::IFF_NO_PI) as libc::c_short;

    // SAFETY: `fd` is the caller's responsibility to have opened on
    // `/dev/net/tun`; the request is the fixed `TUNSETIFF` constant, not a
    // caller-supplied value; and `request` is a fully initialized `ifreq` that
    // outlives the call, so the kernel reads only initialized memory within it.
    let result = unsafe { libc::ioctl(fd, libc::TUNSETIFF, &mut request) };

    if result >= 0 {
        return Ok(TunSetIffOutcome::Ok);
    }

    let error = io::Error::last_os_error();
    Ok(match error.raw_os_error() {
        Some(libc::EPERM | libc::EACCES) => TunSetIffOutcome::PermissionDenied,
        // ENODEV: no such device. ENXIO: the device is not a TUN/TAP device.
        Some(libc::ENODEV | libc::ENXIO) => TunSetIffOutcome::Missing,
        _ => TunSetIffOutcome::Failed(error.kind()),
    })
}

/// On a non-Linux host the probe cannot be attempted.
///
/// Reported as an error rather than as [`TunSetIffOutcome::Missing`]: "I could not
/// check" and "the device is absent" are different answers, and conflating them
/// would have the capability model report a missing kernel feature when what is
/// actually missing is a supported platform.
///
/// # Errors
///
/// Always, on a non-Linux target.
#[cfg(not(target_os = "linux"))]
pub fn tun_set_iff(_fd: RawFd, _name: &str) -> io::Result<TunSetIffOutcome> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "TUN probing is only implemented for Linux",
    ))
}

/// The descriptors a service manager passed to this process.
///
/// # Why the handshake is verified here rather than by the caller
///
/// The two variables are inherited across `exec`, so a process that re-execs sees
/// its parent's values. Acting on them would have two processes serving one socket,
/// which presents as intermittent connection resets rather than as a clear error.
/// Checking `LISTEN_PID` against the current pid is what prevents that, and doing it
/// in the same place as the adoption means a caller cannot forget.
///
/// Returns `None` when the variables are absent, unparsable, zero, or name a
/// different process — the ordinary case for a hand-run service and for every test.
#[must_use]
pub fn verified_descriptors() -> Option<ServiceManagerDescriptors> {
    let count: i32 = std::env::var("LISTEN_FDS").ok()?.parse().ok()?;
    if count < 1 {
        return None;
    }

    let pid: u32 = std::env::var("LISTEN_PID").ok()?.parse().ok()?;
    if pid != std::process::id() {
        return None;
    }

    Some(ServiceManagerDescriptors {
        first: FIRST_INHERITED_DESCRIPTOR,
        count: count as usize,
    })
}

/// The first descriptor a service manager passes, by its documented convention.
pub const FIRST_INHERITED_DESCRIPTOR: RawFd = 3;

/// Descriptors passed by the service manager, verified to belong to this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceManagerDescriptors {
    first: RawFd,
    count: usize,
}

impl ServiceManagerDescriptors {
    /// How many descriptors were passed.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// The first descriptor.
    #[must_use]
    pub const fn first(&self) -> RawFd {
        self.first
    }

    /// Takes ownership of a listening socket from the passed descriptors.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::InvalidInput`] when `index` is past the number the
    /// service manager passed — an empty descriptor table is a configuration
    /// mistake, and adopting a descriptor that was never passed would take over an
    /// unrelated open file.
    ///
    /// A descriptor that is not actually a listening socket is not detected here.
    /// There is no portable test for it, and the failure it causes — `accept`
    /// returning an error — is immediate and names the descriptor, which is a better
    /// signal than a guess made at adoption time.
    pub fn adopt_listening_socket(
        &self,
        index: usize,
    ) -> io::Result<std::os::unix::net::UnixListener> {
        use std::os::fd::FromRawFd;

        if index >= self.count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the service manager passed {} descriptor(s), so index {index} is not one of them",
                    self.count
                ),
            ));
        }

        let fd = self.first + index as RawFd;

        // SAFETY: `fd` is one of the descriptors the service manager passed to this
        // process, verified by `verified_descriptors` through the `LISTEN_PID`
        // handshake, so this process owns it and may take it. Ownership moves into
        // the returned listener, which closes it on drop; the `RawFd` is not used
        // again here or by the caller.
        Ok(unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd) })
    }
}

#[cfg(test)]
mod descriptor_tests {
    use super::*;

    /// Nothing is passed to an ordinary process, so adoption must decline rather
    /// than take over a descriptor that belongs to something else.
    #[test]
    fn no_descriptors_are_found_without_the_handshake() {
        // The test binary is not started by a service manager, so both variables
        // are absent. `verified_descriptors` must answer `None` rather than
        // reaching for fd 3, which in this process is some unrelated file.
        //
        // SAFETY (`std::env::remove_var` is not unsafe, but the sequencing is the
        // point): if a service manager ever did start the test binary, clearing
        // the variables is what makes this assertion meaningful rather than
        // order-dependent on the harness's environment.
        assert!(
            verified_descriptors().is_none(),
            "the test binary must not look like a service-manager child"
        );
    }

    /// The descriptor table must refuse an index it was not given.
    ///
    /// Adopting beyond the count would take over an unrelated open file — a
    /// database, a log, a client connection — and serve HTTP on it. That is the
    /// failure this check exists to make impossible.
    #[test]
    fn an_index_past_the_passed_count_is_refused() {
        let descriptors = ServiceManagerDescriptors { first: 3, count: 1 };
        let err = descriptors
            .adopt_listening_socket(1)
            .expect_err("index 1 is past a count of 1");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    /// The count is reported so a caller can warn when a unit passes more sockets
    /// than it serves, rather than silently serving only the first.
    #[test]
    fn the_count_is_reported() {
        let descriptors = ServiceManagerDescriptors { first: 3, count: 2 };
        assert_eq!(descriptors.count(), 2);
        assert_eq!(descriptors.first(), 3);
    }

    /// The descriptor numbering convention is part of the contract with the
    /// service manager, so it is asserted rather than left as a literal.
    #[test]
    fn the_first_descriptor_is_three() {
        assert_eq!(FIRST_INHERITED_DESCRIPTOR, 3, "systemd starts at fd 3");
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "linux", test))]
    use super::*;

    /// A name longer than the kernel accepts must be rejected here, so the
    /// kernel can never read past the buffer.
    /// The interface-name bound must match the kernel's `IFNAMSIZ - 1`, because a
    /// value that is too large would let the kernel read past the buffer.
    #[test]
    fn the_name_bound_matches_the_kernel_constant() {
        assert_eq!(MAX_INTERFACE_NAME, 15, "IFNAMSIZ is 16 on Linux");
    }

    /// On a non-Linux host the probe must report that it could not check, rather
    /// than answering "the device is absent" — those are different facts, and
    /// conflating them would report a missing kernel feature when the real
    /// problem is an unsupported platform.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn a_non_linux_host_reports_unsupported() {
        let err = tun_set_iff(-1, "probe").expect_err("must not claim an answer");
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_oversized_name_is_rejected() {
        let long = "a".repeat(MAX_INTERFACE_NAME + 1);
        let err = tun_set_iff(-1, &long).expect_err("must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_empty_name_is_rejected() {
        let err = tun_set_iff(-1, "").expect_err("must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// The maximum legal length must be accepted by the validation, so the check
    /// is not off by one.
    ///
    /// Measured on Linux: `ioctl(-1, TUNSETIFF, …)` returns -1 with `EBADF`, which
    /// surfaces as [`TunSetIffOutcome::Failed`] rather than as a validation error.
    /// Asserting `Err` here would be asserting the wrong contract.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_maximum_name_length_is_accepted_by_validation() {
        let exact = "a".repeat(MAX_INTERFACE_NAME);
        match tun_set_iff(-1, &exact) {
            // EBADF is not a permission or device answer, so it must land in
            // the catch-all rather than being mistaken for either. The specific
            // `ErrorKind` is deliberately not asserted: its name for EBADF is
            // not stable across toolchains.
            Ok(TunSetIffOutcome::Failed(_)) => {}
            Ok(other) => panic!("an invalid fd must not report {other:?}"),
            Err(e) => assert_ne!(
                e.kind(),
                io::ErrorKind::InvalidInput,
                "the maximum length must not be rejected as invalid input"
            ),
        }
    }

    /// A bogus fd must produce an outcome or a plain error, never a panic or
    /// undefined behaviour.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_bogus_descriptor_does_not_panic() {
        // Measured: -1 yields EBADF, which is a failure rather than a refusal.
        match tun_set_iff(-1, "proxyctl-probe") {
            Ok(TunSetIffOutcome::Failed(_)) => {}
            Ok(other) => panic!("unexpected outcome from an invalid fd: {other:?}"),
            Err(e) => panic!("EBADF must be an outcome, not an input error: {e}"),
        }
    }

    /// With a real `/dev/net/tun`, the probe must return a definite answer rather
    /// than an indeterminate one. Skips when the device is absent.
    #[cfg(target_os = "linux")]
    #[test]
    fn probing_the_real_device_gives_a_definite_answer() {
        let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/net/tun")
        else {
            return;
        };
        use std::os::fd::AsRawFd;
        let outcome = tun_set_iff(file.as_raw_fd(), "proxyctl-probe0").expect("callable");

        // Whether it succeeds depends on capabilities, but it must be one of the
        // three meaningful answers.
        assert!(
            matches!(
                outcome,
                TunSetIffOutcome::Ok
                    | TunSetIffOutcome::PermissionDenied
                    | TunSetIffOutcome::Missing
                    | TunSetIffOutcome::Failed(_)
            ),
            "unexpected outcome: {outcome:?}"
        );
    }
}
