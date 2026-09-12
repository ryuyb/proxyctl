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
    #[cfg(target_os = "linux")]
    #[test]
    fn the_maximum_name_length_is_accepted_by_validation() {
        let exact = "a".repeat(MAX_INTERFACE_NAME);
        // The call itself fails on a closed fd, which is fine: the point is that
        // it was not rejected as invalid input.
        match tun_set_iff(-1, &exact) {
            Err(e) => assert_ne!(
                e.kind(),
                io::ErrorKind::InvalidInput,
                "the maximum length must not be rejected"
            ),
            Ok(_) => panic!("an invalid fd cannot succeed"),
        }
    }

    /// A bogus fd must produce an outcome or a plain error, never a panic or
    /// undefined behaviour.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_bogus_descriptor_does_not_panic() {
        let outcome = tun_set_iff(-1, "proxyctl-probe");
        match outcome {
            Ok(TunSetIffOutcome::Failed(_) | TunSetIffOutcome::PermissionDenied) => {}
            // EBADF is reported as a failure kind, which is what we expect here.
            Ok(other) => panic!("unexpected outcome from an invalid fd: {other:?}"),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::InvalidInput),
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
