# AGENTS.md

## Project

This repository contains a Linux-native Mihomo management agent for Linux servers and PVE LXC.

Primary goals:

- Manage Mihomo lifecycle and versions.
- Manage Mihomo configuration versions and rollback.
- Manage subscriptions and subscription conversion.
- Provide a Web API and Web administration UI.
- Provide CLI and TUI clients.
- Detect and manage Linux runtime capabilities.
- Support Debian/Ubuntu + systemd + PVE LXC first.
- Keep external integrations replaceable.

Core architectural style:

- DDD-lite
- Hexagonal Architecture / Ports & Adapters
- Modular Monolith
- Agent/Client separation

The project is not intended to reimplement the Mihomo proxy core, a complete Sub-Store, or a complete Mihomo Dashboard.

---

## Non-Negotiable Architecture Rules

### Dependency direction

Dependencies must point inward:

```text
interfaces
    |
application
    |
domain

infrastructure
    |
application ports
```

The bootstrap layer wires everything together.

Allowed:

```text
domain

application -> domain

infrastructure -> application
infrastructure -> domain

interfaces -> application
interfaces -> domain (read-only types where appropriate)

bootstrap -> everything needed for composition
```

Forbidden:

```text
domain -> application
domain -> infrastructure
domain -> interfaces

application -> infrastructure
application -> axum
application -> reqwest
application -> sqlx
application -> systemd
application -> nftables

domain -> tokio
domain -> reqwest
domain -> serde implementations
domain -> filesystem/process/network details
```

The Domain layer must remain independent from Linux, Mihomo, HTTP, databases, and UI frameworks.

### Domain purity

Domain code should model:

- entities
- value objects
- domain invariants
- domain errors
- pure business rules

Do not put these in Domain:

- HTTP clients
- filesystem operations
- command execution
- process spawning
- database queries
- systemd integration
- nftables/iptables calls
- environment variable parsing
- API DTOs
- CLI/TUI rendering

### Application layer

Application owns use cases and orchestration.

Examples:

- StartMihomo
- StopMihomo
- RestartMihomo
- ReloadMihomo
- UpdateMihomo
- UpdateSubscription
- ActivateConfig
- RollbackConfig
- ValidateConfig
- RunDoctor

Application depends on Ports, not implementations.

### Infrastructure layer

Infrastructure implements Ports.

Examples:

- Mihomo HTTP/Unix socket controller
- Mihomo process manager
- Sub-Store adapter
- sub-store-convert adapter
- native converter
- SQLite repositories
- filesystem stores
- scheduler
- systemd
- nftables
- iptables
- PVE/LXC detection

### Interface layer

Interfaces are adapters.

Examples:

- REST API
- WebSocket
- CLI
- TUI
- Web UI integration

They must call Application use cases instead of implementing business logic themselves.

---

## Repository Layout

Preferred workspace:

```text
proxy-manager/
├── Cargo.toml
├── crates/
│   ├── domain/
│   ├── application/
│   ├── infrastructure/
│   ├── interfaces/
│   └── bootstrap/
├── frontend/
│   ├── admin/
│   └── metacubexd/
├── migrations/
├── packaging/
│   ├── systemd/
│   ├── deb/
│   └── install.sh
├── tests/
├── docs/
│   ├── architecture.md
│   ├── research/
│   └── adr/
└── AGENTS.md
```

Do not reorganize the project into a different architecture without an ADR.

---

## Domain Boundaries

Primary bounded areas:

```text
domain/
├── mihomo/
├── subscription/
├── configuration/
└── system/
```

### Mihomo

Owns domain concepts such as:

- MihomoInstance
- MihomoInstanceId
- MihomoVersion
- MihomoStatus
- MihomoRuntimeStatus

### Subscription

Owns:

- Subscription
- SubscriptionId
- SubscriptionSource
- SubscriptionProfile
- ConverterId
- Schedule

### Configuration

Owns:

- ConfigVersion
- ConfigVersionId
- ConfigSource
- ConfigChecksum
- ValidationResult
- Activation state

### System

Owns:

- Platform
- Architecture
- InitSystem
- ContainerEnvironment
- CapabilityStatus
- NetworkCapabilities

Do not introduce generic "manager/service/repository" abstractions without a concrete domain purpose.

---

## Ports

Ports are defined by the Application layer.

Typical Ports:

```rust
#[async_trait]
pub trait MihomoController: Send + Sync {
    async fn status(&self) -> Result<MihomoRuntimeStatus>;
    async fn reload(&self, config: ConfigPath) -> Result<()>;
    async fn proxies(&self) -> Result<ProxyList>;
    async fn connections(&self) -> Result<ConnectionList>;
    async fn traffic(&self) -> Result<TrafficStats>;
}
```

```rust
#[async_trait]
pub trait SubscriptionConverter: Send + Sync {
    async fn convert(
        &self,
        request: ConvertRequest,
    ) -> Result<ConvertedSubscription>;
}
```

```rust
#[async_trait]
pub trait ConfigRepository: Send + Sync {
    async fn list(&self) -> Result<Vec<ConfigVersion>>;
    async fn get(&self, id: ConfigVersionId) -> Result<Option<ConfigVersion>>;
    async fn save(&self, config: ConfigVersion) -> Result<()>;
    async fn activate(&self, id: ConfigVersionId) -> Result<()>;
}
```

Keep Ports small and capability-oriented.

Avoid giant interfaces such as:

```rust
trait SystemManager {
    // 40 unrelated methods
}
```

Prefer focused Ports.

---

## Subscription Architecture

Subscription conversion must be replaceable.

Target abstraction:

```text
SubscriptionConverter
├── SubStoreConverter
├── SubStoreConvertAdapter
└── NativeConverter
```

Application code must depend only on:

```text
SubscriptionConverter
```

It must not know:

- Sub-Store endpoint paths
- Node/Bun
- sub-store-convert internals
- Sub-Store database structures

### Integration rule

Prefer documented/public behavior over internal implementation details.

Treat external services as replaceable.

Never spread external API URL construction throughout the codebase. Keep it inside the adapter.

### Failure behavior

A failed subscription update must never destroy the currently active config.

Required flow:

```text
current active config
        |
subscription update
        |
conversion
        |
validation
        |
new config version
        |
activate
        |
reload Mihomo
        |
health check
        |
success
```

On failure:

```text
old config remains active
```

If activation or reload succeeds partially and health check fails, attempt rollback.

---

## Configuration Lifecycle

Configurations are immutable versions.

Recommended model:

```text
/var/lib/proxy-agent/configs/
├── v001.yaml
├── v002.yaml
├── v003.yaml
└── v004.yaml

active -> configs/v004.yaml
```

Never edit the active configuration in place.

Prefer:

```text
write temporary file
    |
flush/sync as appropriate
    |
atomic rename
    |
activate
```

Every activated config must have a version identifier and checksum.

Config update must support:

- list
- show
- validate
- diff
- activate
- rollback

---

## Mihomo Integration

Mihomo is the data plane.

The Agent is the control plane.

Keep these responsibilities separate:

```text
Mihomo:
    proxy runtime

Agent:
    lifecycle
    configuration lifecycle
    subscriptions
    scheduling
    system integration
    API/TUI/CLI
```

Prefer a Unix socket for local Mihomo control when supported by the deployment.

If using an HTTP controller, default to localhost rather than public binding.

Do not expose Mihomo's controller to the Internet by default.

---

## Process Management

Process execution belongs in Infrastructure.

Do not invoke commands from:

- Domain
- Application use-case logic
- HTTP handlers
- TUI widgets
- CLI command implementations

Use a Port such as:

```rust
trait ProcessManager
```

and implement it in Infrastructure.

If systemd becomes the authoritative process manager, encapsulate that behind an adapter.

---

## Linux Runtime

Initial supported environment:

```text
Debian
Ubuntu
systemd
PVE LXC
```

Do not claim broad distribution compatibility until tested.

Linux-specific features must be detected rather than assumed.

Examples:

- `/dev/net/tun`
- `CAP_NET_ADMIN`
- nftables
- iptables
- routing
- systemd
- LXC
- kernel capabilities

Use explicit capability states:

```text
Supported
Unsupported
Unavailable
Misconfigured
Unknown
```

Do not reduce environment compatibility to a boolean.

---

## PVE LXC

PVE LXC support is capability-based.

Never assume:

```text
LXC == TUN available
```

or:

```text
LXC == CAP_NET_ADMIN available
```

The `doctor` functionality must detect actual runtime capabilities.

Privileged and unprivileged LXC must be treated separately.

A feature being unavailable must not cause unrelated Mihomo functionality to fail.

Example:

```text
HTTP/SOCKS/Mixed proxy    available
TUN                       unavailable
nftables transparent mode unavailable
```

That is a valid degraded state.

---

## Security Rules

### Privilege separation

The Agent may require privileged operations, but Web/API handlers must not expose arbitrary shell execution.

Never implement:

```rust
POST /api/run-command
```

or equivalent arbitrary command endpoints.

All privileged operations must map to explicit Application use cases and Ports.

### Mihomo controller

Default to:

```text
127.0.0.1
```

or a Unix socket.

Do not bind Mihomo's controller to `0.0.0.0` unless the deployment explicitly requires it and proper authentication/firewall protections exist.

### Unix socket

Treat socket filesystem permissions as part of the security model.

### Web authentication

Local Unix socket access may rely on OS permissions.

Web access should use authentication when remotely reachable.

Do not log:

- subscription URLs containing credentials
- Mihomo secrets
- authentication tokens
- proxy credentials
- full sensitive config contents

Redact sensitive fields in logs.

---

## Error Handling

Use typed errors at architectural boundaries.

Preferred:

```text
DomainError
ApplicationError
InfrastructureError
API Error
CLI/TUI presentation error
```

Use `thiserror` for structured errors.

`anyhow` may be used at application/bootstrap boundaries where context aggregation is useful, but do not erase important domain/application error types unnecessarily.

Never panic on normal runtime failures.

`unwrap()` / `expect()` should be limited to truly impossible states and initialization invariants, with a clear reason.

---

## Async and Concurrency

Use Tokio for asynchronous runtime.

Do not block the Tokio runtime with blocking system operations.

Use:

```rust
tokio::task::spawn_blocking
```

when a blocking operation cannot be avoided.

Avoid spawning unbounded background tasks.

Background tasks must have:

- cancellation behavior
- error handling
- structured logging
- clear ownership

Schedulers must not create duplicate concurrent subscription updates for the same subscription.

---

## State and Concurrency

For each Mihomo instance, lifecycle operations must be serialized.

Invalid transitions should be rejected.

Example:

```text
Starting -> Starting
```

should not trigger another process spawn.

Similarly, concurrent config activation must be coordinated.

Preferred approach:

```text
per-instance lock
```

or an equivalent serialized command mechanism.

---

## TUI Rules

TUI is an Interface Adapter.

Do not place business logic in:

```text
ratatui widgets
screen rendering
keyboard handlers
```

Keyboard actions should dispatch Application commands or local client commands.

TUI should be able to operate through the same Agent API used by CLI clients where practical.

Recommended:

```text
proxyctl
   |
Unix socket
   |
proxy-agent
```

Do not make TUI directly manage Mihomo processes.

TUI should prioritize:

- overview
- runtime status
- proxy groups
- logs
- subscription updates
- config rollback
- system doctor

Complex configuration editing belongs in Web UI.

---

## CLI Rules

CLI commands must remain automation-friendly.

Examples:

```bash
proxyctl start
proxyctl stop
proxyctl restart
proxyctl reload
proxyctl status
proxyctl logs
proxyctl logs -f

proxyctl mihomo version
proxyctl mihomo update

proxyctl subscription list
proxyctl subscription update

proxyctl config list
proxyctl config validate
proxyctl config rollback 41

proxyctl doctor
```

Where practical, support machine-readable output:

```bash
proxyctl status --json
```

Do not print secrets by default.

Exit codes must distinguish success from operational failure.

---

## Web API Rules

Use versioned API paths:

```text
/api/v1/...
```

Never expose Domain entities directly as API responses.

Use DTOs.

Typical endpoints:

```text
GET    /api/v1/system
GET    /api/v1/health
GET    /api/v1/doctor

GET    /api/v1/mihomo
POST   /api/v1/mihomo/start
POST   /api/v1/mihomo/stop
POST   /api/v1/mihomo/restart
POST   /api/v1/mihomo/reload

GET    /api/v1/configs
POST   /api/v1/configs/:id/activate
POST   /api/v1/configs/:id/rollback
POST   /api/v1/configs/validate

GET    /api/v1/subscriptions
POST   /api/v1/subscriptions
PATCH  /api/v1/subscriptions/:id
DELETE /api/v1/subscriptions/:id
POST   /api/v1/subscriptions/:id/update
```

WebSocket:

```text
/ws/v1/events
```

Keep API transport concerns out of Application.

---

## Events

MVP event mechanism:

```rust
tokio::sync::broadcast
```

Possible internal events:

```text
MihomoStatusChanged
MihomoLog
TrafficUpdated
SubscriptionUpdated
ConfigActivated
JobFinished
SystemCapabilityChanged
```

Events are for notifications and coordination.

Do not introduce Event Sourcing.

Do not add Kafka, NATS, RabbitMQ, or another external event broker for the MVP.

---

## Persistence

SQLite stores metadata.

Filesystem stores large/versioned config files.

Recommended:

```text
/etc/proxy-agent/
    config.toml

/var/lib/proxy-agent/
    configs/
    subscriptions/
    cache/
    state/
    database.sqlite

/run/proxy-agent/
    agent.sock
    mihomo.sock
```

Do not put full generated YAML into SQLite unless there is a concrete requirement.

Repository APIs should express domain intent rather than SQL mechanics.

---

## Logging

Use:

```text
tracing
tracing-subscriber
```

Prefer structured fields:

```text
subscription_id
config_version
mihomo_instance_id
job_id
```

Example:

```text
INFO subscription_id=sub_01 config_version=42 event=subscription_updated
```

Never log secrets or full subscription credentials.

---

## Testing

### Domain

Pure unit tests.

No filesystem, process, network, or real Mihomo.

### Application

Use mocked Ports.

Must cover at least:

```text
subscription update success
subscription conversion failure
validation failure
config activation failure
Mihomo reload failure
rollback success
rollback failure
invalid Mihomo lifecycle transition
concurrent update prevention
```

### Infrastructure

Integration tests against real infrastructure where practical:

- Mihomo
- SQLite
- filesystem
- Sub-Store adapter
- systemd test environment

### E2E

At minimum:

```text
install
doctor
start
status
subscription update
config activation
Mihomo health
dashboard
rollback
```

---

## Quality Gates

Before merging Rust changes:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --workspace
```

When appropriate:

```bash
cargo nextest run
cargo audit
cargo deny check
```

Do not suppress Clippy warnings without a concrete justification.

---

## Dependency Policy

Add dependencies only when they solve a concrete problem.

Before adding a crate:

1. Verify maintenance status.
2. Verify license.
3. Check whether the standard library or an existing dependency is sufficient.
4. Check whether the dependency introduces a large runtime or transitive tree.
5. Document non-obvious architectural dependencies.

Avoid adding heavy framework abstractions merely for convenience.

Preferred baseline:

```text
tokio
axum
tower
tower-http
reqwest
serde
serde_json
toml
serde_yaml / serde_yml
clap
ratatui
crossterm
tracing
tracing-subscriber
async-trait
thiserror
sqlx
```

The exact crate versions must be chosen from current stable releases during implementation; do not copy stale versions from this document.

---

## External Projects and Licenses

Treat these projects as external dependencies/components:

```text
Mihomo
Sub-Store
sub-store-convert
metacubexd
ShellCrash
```

Do not copy external source code into the repository unless license compatibility and attribution requirements have been reviewed.

Sub-Store is licensed under AGPL-3.0.

Mihomo and other third-party components may have different licenses.

Record license conclusions in:

```text
docs/research/13-licenses.md
```

and relevant ADRs.

When uncertain, stop and record the question rather than guessing.

---

## Research Before Implementation

Before making major architectural decisions, consult:

```text
docs/research/
docs/adr/
docs/research/open-questions.md
```

Phase 0 research covers:

```text
Mihomo API
Mihomo configuration
Mihomo runtime
Sub-Store API
Sub-Store deployment
sub-store-convert
metacubexd
ShellCrash
Linux runtime
PVE LXC
nftables / iptables
security
licenses
deployment
competitors
```

Do not turn an unverified assumption into a hard dependency.

When external behavior matters, verify it against current upstream documentation or source.

---

## ADR Policy

Architectural changes require an ADR when they affect:

- dependency direction
- bounded contexts
- Port definitions
- external integration strategy
- persistence model
- security model
- deployment model
- process supervision model
- network/TUN model
- Sub-Store integration
- Dashboard strategy

Use:

```text
docs/adr/ADR-NNN-title.md
```

An ADR should contain:

```text
Status
Context
Decision
Alternatives
Consequences
```

---

## Development Workflow

For a new feature:

```text
1. Check requirements and research.
2. Identify the bounded context.
3. Define/adjust Domain model.
4. Define Application use case.
5. Define required Ports.
6. Implement Infrastructure adapters.
7. Implement API/CLI/TUI adapter.
8. Add tests.
9. Update ADR/documentation if architecture changed.
```

Do not start with a Web handler or CLI command and then leak infrastructure concerns downward.

---

## Commit Scope

Prefer focused commits.

Good:

```text
feat(domain): add config version model
feat(application): add rollback use case
feat(infrastructure): add sqlite config repository
feat(api): expose config rollback
test(application): cover rollback failure
```

Avoid giant commits mixing:

```text
domain + UI + installer + systemd + refactor
```

---

## Definition of Done

A feature is complete only when:

- Domain rules are implemented in Domain where applicable.
- Application orchestration is testable independently.
- Infrastructure details are behind Ports.
- API/CLI/TUI do not contain duplicated business logic.
- Failure behavior is defined.
- Logs do not leak secrets.
- Tests exist for important success and failure paths.
- Documentation is updated for architectural changes.
- `cargo fmt`, `cargo clippy`, and `cargo test` pass.

---

## MVP Scope Reminder

The first production-oriented version should target:

```text
Debian/Ubuntu
systemd
PVE LXC
x86_64
aarch64
```

Core features:

```text
Mihomo lifecycle
Mihomo update
Config versioning
Config validation
Config rollback
Subscription management
Sub-Store integration
REST API
Web admin
metacubexd integration
CLI
TUI
Doctor
systemd packaging
```

Defer unless required by evidence:

```text
OpenRC
multi-instance management
remote multi-node control
advanced nftables automation
advanced TProxy automation
full native subscription converter
Kubernetes
microservices
```

---

## Product Philosophy

The system should prefer:

```text
reliability > feature count
explicit capabilities > assumptions
rollback > destructive update
replaceable adapters > hard dependencies
simple modular monolith > premature microservices
upstream reuse > reimplementation
observable behavior > magic automation
```

The most important invariant is:

> A failed update, conversion, reload, or system capability check must degrade safely and preserve the last known-good working configuration whenever possible.
