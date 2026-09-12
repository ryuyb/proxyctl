//! System environment and runtime capabilities.
//!
//! This module *interprets* probe results; it never performs probing. Probes
//! (syscalls, `/proc` reads, `nft` trial writes) live in infrastructure and feed
//! their observations in as plain data.
//!
//! The central rule, established by measurement during Phase 0:
//!
//! > TUN availability requires device access **and** `CAP_NET_ADMIN`. A device
//! > node that exists and can be `open()`ed still fails `ioctl(TUNSETIFF)` with
//! > `EPERM` when the capability is missing.
//!
//! That is why capability state is a five-value enum rather than a boolean, and
//! why [`Misconfigured`] exists as a distinct outcome: prerequisites are present
//! but the feature does not work, which is a different fix from "not supported".
//!
//! [`Misconfigured`]: CapabilityStatus::Misconfigured

pub mod capability;
pub mod doctor;
pub mod environment;

pub use capability::{
    Capability, CapabilityEvidence, CapabilityKind, CapabilitySet, CapabilityStatus,
};
pub use doctor::{DoctorConclusion, DoctorReport, NetworkDoctorSection};
pub use environment::{
    Architecture, ContainerEnvironment, InitSystem, OperatingSystem, Privilegedness,
    SystemEnvironment,
};
