//! Host integration: capability detection and init-system observation.

pub mod capabilities;
pub mod services;

pub use capabilities::LinuxCapabilityProbe;
pub use services::SystemdServiceManager;
