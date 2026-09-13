//! Port definitions.
//!
//! The application layer declares the capabilities it needs; adapters implement
//! them. Ports are grouped by capability rather than by external system, so a
//! trait never becomes a mirror of some third-party API surface.
//!
//! Every port is `Send + Sync` and object-safe, because the bootstrap layer
//! assembles `Arc<dyn Port>` values at runtime. This is why the async methods
//! use [`async_trait`](async_trait::async_trait): native async functions in
//! traits are not yet usable behind a trait object.

pub mod audit_sink;
pub mod capability_probe;
pub mod clash_proxy;
pub mod config_repository;
pub mod config_validator;
pub mod error;
pub mod event_publisher;
pub mod instance_repository;
pub mod job_registry;
pub mod kernel_installer;
pub mod mihomo_connection_ops;
pub mod mihomo_controller;
pub mod mihomo_observer;
pub mod process_manager;
pub mod secret_store;
pub mod service_manager;
pub mod session_store;
pub mod subscription_converter;
pub mod subscription_repository;
pub mod types;

pub use audit_sink::AuditSink;
pub use capability_probe::{CapabilityProbe, ProbeOptions};
pub use config_repository::ConfigRepository;
pub use config_validator::{ConfigValidator, PreflightContext};
pub use error::{ConverterError, PortError};
pub use event_publisher::{DomainEvent, EventPublisher};
pub use instance_repository::InstanceRepository;
pub use job_registry::{
    Degradation, JobKind, JobRecord, JobRegistry, JobState, JobStep, JobTarget,
};
pub use kernel_installer::KernelInstaller;
pub use mihomo_connection_ops::{ConnectionList, ConnectionView, MihomoConnectionOps};
pub use mihomo_controller::{MihomoController, ReloadRequest};
pub use mihomo_observer::{BoxStream, LogEntry, MemorySample, MihomoObserver, TrafficSample};
pub use process_manager::{
    AllowedSignal, ExitStatus, ProcessHandle, ProcessManager, ProcessStatus, StartOptions,
};
pub use secret_store::{Principal, SecretStore};
pub use service_manager::ServiceManager;
pub use session_store::{SessionId, SessionPolicy, SessionStore};
pub use subscription_converter::{ConvertRequest, SubscriptionConverter};
pub use subscription_repository::SubscriptionRepository;
pub use types::{
    CachePolicy, ConverterCapabilities, ConverterHealth, DelayOptions, DelayOutcome,
    DownloadedArtifact, HealthReport, KernelInstallation, LogLevel, ProxyGroupView, ProxyList,
    ProxyView, ReloadOutcome, RuleList, RuleView, RuntimeConfigSummary,
};
