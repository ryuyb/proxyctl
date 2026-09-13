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

The project is not intended to reimplement the Mihomo proxy core, a complete
Sub-Store, or a complete Mihomo Dashboard — the dashboard is **embedded as an
upstream artifact** and served alongside this repository's own interface. Both are
described under "Web Interfaces".

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
application -> rusqlite
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
- streaming NDJSON endpoints (events, logs)
- CLI
- TUI
- Web UI integration
- same-origin Clash API relay for the embedded dashboard

They must call Application use cases instead of implementing business logic themselves.

---

## Repository Layout

The actual workspace. `crates/` holds the Rust workspace; `proxyctl` is one
binary with both a client and an agent role.

```text
proxyctl/
├── Cargo.toml                  workspace manifest
├── crates/
│   ├── domain/                 pure rules; no I/O, no async runtime
│   ├── application/            use cases and Ports
│   ├── infrastructure/         Port implementations (SQLite, mihomo, systemd)
│   ├── interfaces/             HTTP API, embedded UI, the Clash API relay
│   ├── cli/                    proxyctl: client commands, TUI, agent role
│   └── bootstrap/              composition root
├── frontend/
│   ├── admin/                  this repository's operator interface (Vite + React)
│   └── metacubexd/             the upstream dashboard artifact, fetched not built
├── packaging/
│   ├── systemd/proxy-agent.service
│   └── config.toml.example
├── scripts/
│   ├── fetch-metacubexd.sh     fetches the dashboard artifact
│   └── gen-config-whitelist.py regenerates the mihomo field list from upstream
├── docs/
│   ├── design/                 per-feature design notes
│   ├── research/               Phase 0 research
│   └── adr/
└── AGENTS.md
```

**There is no `migrations/` or top-level `tests/`.** Both were in an earlier
sketch, and neither is where the work ended up:

* schema changes are versioned by `PRAGMA user_version` in
  `crates/infrastructure/src/storage/schema.rs`, so there are no migration files
  to keep in a directory. A future build reads that number to tell an old
  database from a current one;
* tests live beside what they test — unit tests in each crate, integration tests
  in that crate's `tests/` — because a top-level `tests/` would need the whole
  workspace as a dependency and would hide which crate a failure belongs to.

`packaging/` holds the systemd unit and the documented configuration example. The
deb and install script are planned, not yet written.

`frontend/metacubexd/dist/` is build output and is not committed; run
`scripts/fetch-metacubexd.sh` first. A `cargo build` without it succeeds and
embeds a placeholder page.

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

Treat socket filesystem permissions as part of the security model — but note
that the two sockets have **opposite** requirements, and conflating them is the
mistake to avoid.

**The agent socket does not rely on permissions.** It is mode `0666` and any local
user may connect; being able to connect *is* being an operator. The trade is
deliberate: the client is meant to work without anyone being added to a dedicated
group. On a multi-user host that means every local user can manage the kernel, so
a deployment that cares must narrow it — either the socket mode or a peer check.

**The socket is created by the service manager, not the agent.** `proxy-agent.socket`
listens on the agent's path and starts `proxy-agent.service` on the first
connection, which is what removes `sudo systemctl` from the operator's path: asking
someone to start a unit needs root, because polkit's `manage-units` default is
`auth_admin` and an SSH session has no prompt. Three consequences are load-bearing:

* the agent adopts the descriptor from `LISTEN_FDS` (verified against `LISTEN_PID`)
  instead of calling `bind`, and must not unlink what is already at the path — that
  file belongs to systemd, and removing it breaks the next start rather than the
  current one;
* the *service* unit declares `RuntimeDirectory=` because a socket unit ignores
  `User=`, so a directory created there is `root:root` and the unprivileged service
  cannot chmod or clear the socket inside it. Measured: it crash-looped;
* that directory needs `RuntimeDirectoryPreserve=yes`, or stopping the service
  deletes the socket unit's listening file while the socket unit still reports
  itself `active`. Measured: the next client got `ECONNREFUSED` with no recovery
  short of restarting the socket unit.

The directory check in `composition.rs` compares modes rather than ownership for
the same reason: requiring the service user to own a directory systemd created as
root made a correct deployment refuse to start.

**The kernel socket is not protected, and that is a known accepted risk.** Mihomo
hardcodes `chmod 0666` on its controller socket and does not verify its `secret`
over a unix socket, so `mihomo.sock` is world-writable and the agent cannot tighten
it — measured, not inferred. Upstream's own source has the `os.Chmod(addr, 0o666)`;
`crates/infrastructure/src/mihomo/socket.rs` has a `tighten_socket` that would set
`0660`, but nothing calls it.

This matters *because* the runtime directory became `0751` to let any local user
reach the authenticated agent socket. That also lets any local user reach the
kernel's socket, which is unauthenticated:

```text
curl -X PUT --unix-socket /run/proxy-agent/mihomo.sock -d '{}' http://localhost/configs
```

Verified as an unprivileged user: `GET /version` returns `200` and `PUT /configs`
returns `204`, so any local user can replace the running kernel configuration. Before
the socket was opened, the `0750` directory was what prevented this; now nothing
does. The two requirements are genuinely in conflict when the sockets share a
directory, and separating them (`kernel/mihomo.sock` under `0750`) is the fix if
that risk is ever worth removing. It is deliberately not applied today: the target
deployments are single-user hosts and containers, where the local users are the
operator.

Do not describe the kernel socket as protected by its mode. It is not.

### Web authentication

Two transports, two mechanisms, and one rule they share: **a failure to verify is
a refusal, never a pass.**

**Unix socket.** Any local user reaches it (mode `0666`); the agent does not treat
the file permissions as the boundary. The peer credential (`SO_PEERCRED`) is an
*optional additional* check and is off by default — deliberately, because LXC uid
mapping can make a correct peer look wrong, and a check that locks an operator out
of their own agent is worse than the risk it addresses. When a deployment does
configure a uid or gid, a read failure is a refusal: allowing it would make the
check bypassable by breaking the read.

**TCP.** Over a network the token *is* the identity. A token is required, with
**no loopback exemption** — loopback is not a trust boundary on a host that also
runs untrusted software. Both transports serve at once; configuring a bind adds a
listener, it does not replace the socket.

**There is no role.** Authentication is the only authorization dimension: every
caller that gets past it may do everything this interface offers. There was once an
`Admin`/`ReadOnly` split, removed because it could only be enforced on the TCP path
— the socket admits any local user by design — while no shipped client ever used
the narrower level. A model enforced on one of two transports is worse than none,
because it reads like a boundary that is not there. What survives of it is a
**method** gate on `/clash-api` (`GET`/`HEAD`/`OPTIONS` only), which guards against
a page triggering a kernel config replacement, not against a caller's identity.

**Browsers get a session cookie, not a token.** A page cannot safely hold a token:
whatever a page holds is readable by any script running on it, and this interface
renders operator-supplied content. Sign-in exchanges a token for an `HttpOnly`
cookie the browser refuses to hand to JavaScript:

```text
HttpOnly; SameSite=Strict; Path=/; Max-Age=43200   (+ Secure only behind TLS)
```

There is deliberately **no CSRF token** — injecting one into the page would put a
credential where a script can read it, which is the problem the design exists to
avoid. `SameSite=Strict` plus a same-origin check on state-changing requests
covers the same ground. The check runs **only on the cookie path**, not as a
global middleware: a script holding a token sends no `Origin` and must not be
refused for it.

Tokens and session identifiers are stored as SHA-256 with a per-row salt, not
Argon2 or bcrypt. Both are 256-bit random values, so there is no dictionary to
attack, and a deliberately slow hash would tax every request. All credential
comparisons are constant-time with no early exit.

`Secure` is set only when TLS is actually terminating in front of the listener.
Setting it unconditionally would make the cookie unusable on the loopback and
private-network deployments this agent targets.

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
proxyctl subscription add NAME URL
proxyctl subscription update

proxyctl config list
proxyctl config add FILE
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

**Storing a configuration is not activating one, and the difference is the
kernel.** `config add` validates, persists, and moves the active pointer; it does
*not* reload. That is what lets a first configuration exist at all, since there is
no kernel to reload yet — and a start reads the active pointer to decide what to
launch, so a stored version is immediately startable. `config activate` is the one
that reloads a *running* kernel and rolls back if it is rejected.

Routing `add` through `activate` looks like reuse and is a trap. It fails at the
reload, treats that as a rollback, and reports "stored, but rejected" for a document
that was valid and *had* been stored — found exactly that way, because the first
attempt at this command delegated to activation.

Because of this, launch options cannot be a snapshot taken at agent startup. An
agent already running when the first version was stored still held `None`, so a
start reported "no configuration is active" while `config list` showed one that was.
`StartMihomo` re-derives the config path from the active pointer on every start and
takes only the host-level fields (`binary_path`, `working_dir`) from the cached
options — the application layer must not invent a configs directory layout, which is
the adapter's decision.

**A `200` can still be a failure.** `config validate` is the case: the agent
answers `200` because the request succeeded and the answer is what the document
is, so the status says nothing about the document. A command in that position
reports its own exit code via `Command::exit_code`; returning `Success` because
the transport succeeded made `config validate && config activate` activate a
rejected document. `render` returning `Err` means something different — that the
body was not the shape the command expects — so the two must not be conflated.

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

The event stream:

```text
/ws/v1/events
```

**The path says `ws`; the transport is not WebSocket.** It is newline-delimited
JSON over a chunked HTTP response. The path keeps its name from the design
document, and there is no handshake and no framing.

Three reasons, verified during implementation:

* the server is HTTP/1.1 without upgrade support on this route, and implementing
  a handshake, framing, masking, and ping/pong is a protocol implementation
  rather than glue;
* the stream is **one-directional** — the agent pushes, and everything a client
  *does* goes through the REST endpoints — so WebSocket's distinguishing
  capability is unused;
* `fetch()` with a `ReadableStream` reads a chunked body incrementally on every
  current engine, which was the historic reason to reach for WebSocket.

A heartbeat is sent periodically, because a stream that is quiet for minutes
looks like an idle connection to a proxy and gets cut. The Clash API relay *does*
need real upgrade support, for the kernel's own WebSocket endpoints; that is a
separate path under `/clash-api` and is described below.

Keep API transport concerns out of Application.

---

## Web Interfaces

The agent serves **two** browser interfaces, and they are not merged on purpose.

```text
/            this repository's operator interface (embedded; history routing)
/ui/*        the upstream metacubexd dashboard (embedded; hash routing)
/clash-api/* the dashboard's data feed, relayed to the kernel
/api/control → 404
```

The operator interface owns this repository's vocabulary: lifecycle,
configuration versions, subscriptions, capabilities, doctor, connections. The
dashboard is a view over the kernel's own Clash API — proxy-group switching, node
latency, the traffic graph, rule inspection. Reimplementing it was declined in
ADR-001, so it is reused as a built artifact.

### The dashboard is fetched, not built

`scripts/fetch-metacubexd.sh` pulls upstream's published `gh-pages` artifact into
`frontend/metacubexd/dist/`, which is **not committed**. Reasons:

* building it here would make the Rust build depend on a Node toolchain and on
  Google Fonts, so an offline or sandboxed `cargo build` would fail for a reason
  unrelated to the agent;
* upstream already publishes exactly this directory as its static release, so
  consuming that avoids forking someone else's Nuxt app.

A `cargo build` without the artifact succeeds and serves a placeholder page naming
the script. `frontend/metacubexd/UPSTREAM_VERSION` records the tag in use and *is*
committed, so a checkout states which version it expects **and the script reads it
as the default**. That is why the fetch is reproducible and why CI needs no API
call: an unauthenticated lookup is rate-limited per source address, a runner
shares one, and resolving "latest" per build would let two builds of the same
commit embed different dashboards. `--latest` is the explicit way to move the pin.

### `/api/control` must stay a 404

Upstream probes `GET /api/control/info` to decide whether it is running beside a
*control agent* — upstream's own supervisor, profile store, and kernel installer.
Any error puts it in plain-panel mode and hides its Profile and kernel-control
pages, which is what we want: running both would mean two processes that each
believe they own the kernel (ADR-006 C4).

The path is registered as a handler that returns `404` rather than left
unmatched, because an unmatched path reaches the single-page fallback and would
answer `200` with HTML — which the probe would read as an agent that exists with
no features.

### `/clash-api` is the only browser path to the kernel

The relay exists because three things would otherwise be required, and each is
worse than the relay:

* **the kernel secret would reach the browser.** A page holding it can call
  `PUT /configs`, which replaces the running configuration — a larger grant than
  any endpoint this agent exposes, and one that cannot be revoked without
  restarting the kernel;
* **the controller would have to be reachable.** The default transport is a unix
  socket precisely so it is not, and a browser cannot open one anyway;
* **the dashboard would be cross-origin**, requiring the kernel's own
  `external-controller-cors` to be loosened.

So: the browser talks to the agent's origin, the real secret is injected by the
relay, and the controller stays on its socket.

Two rules that must not be relaxed:

1. **Authorization is per method, not per caller.** `GET`, `HEAD`, and `OPTIONS`
   are relayed; every other method is refused with `403`. There is no privileged
   caller to allow — see "Web authentication" — so the gate is the method itself.
   It exists because a browser page must not be able to reach `PUT /configs`,
   which replaces the running configuration. Verified against upstream's API
   usage: every write it makes is a `PUT` or `POST`, and no `GET` changes kernel
   state. An unrecognised method is treated as a write.
2. **The real secret must never be disclosed to a page.** The generated
   `config.js` sets `defaultBackendURL` and nothing else — in particular no
   `controlToken`, which is how upstream's own server leaks its agent token into
   the browser. Do not follow that pattern.

### WebSocket upgrades are relayed, not implemented

The dashboard's traffic, connections, memory, and logs pages are WebSockets. The
relay forwards the upgrade and then moves bytes with `copy_bidirectional`; frames
are never parsed. Implementing WebSocket to forward it would be a protocol to get
wrong at every detail while adding nothing.

Three things this cost to learn, each of which made *every* stream fail:

* `serve_connection` must call `.with_upgrades()`, or hyper answers `101` and then
  drops the socket;
* the browser's `sec-websocket-key` must be forwarded. The kernel derives
  `sec-websocket-accept` from it and the browser verifies that value against the
  key it sent, so a relay that mints its own key produces a handshake every
  browser rejects;
* `Connection` is **not** stripped from the `101` response, though it *is* stripped
  from the request. A `101` is only valid when it says `Connection: Upgrade`.

Because a relay's failures live between two sockets, its tests must assert what
the far end **received** — not that the relay returned without error. A test that
only checks for `101` passes with all three bugs above present.

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

The documented layout for a packaged install:

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

Every one of those paths is **configurable**, and nothing should hard-code them.
`[paths]` in the configuration file sets the three directories; the agent socket
comes from `[agent] socket`. This matters for more than taste: the agent runs in
tests and in development checkouts rooted somewhere else entirely, and a compiled-in
`/var/lib/proxy-agent` would make that impossible. `proxyctl agent run
--print-config` reports the effective values and which source supplied each.

`config.toml` must be mode `0600`. The loader refuses to start otherwise, because
the file may hold the kernel secret — a group- or world-readable secret file is a
worse failure than a refusal to start.

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

The `dashboard` case means more than "the page loads". Verified by hand against a
real kernel, and worth repeating before a release:

```text
/ui          serves the dashboard, not the placeholder page
/ui/         the same (the trailing-slash route is registered separately)
/clash-api/version      relays to the kernel
/clash-api/{traffic,connections,memory,logs}   all four upgrade to 101
/api/control/info       404, so upstream hides its control pages
method gate             GET and WebSocket allowed, PUT and POST refused with 403
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

Before merging changes to `frontend/admin`:

```bash
pnpm --dir frontend/admin check    # typecheck, lint, test, build
```

That gate is a separate toolchain, and it is **not** part of `cargo test`. A
front-end change with a broken type or a missing translation therefore passes the
Rust gates; run it when the diff touches `frontend/admin/`.

The dashboard is a fetched artifact with no sources in this tree, so it has no
gate of its own. What must be checked when the artifact is involved is the Rust
side: `crates/interfaces/src/http/upgrade.rs` and
`crates/infrastructure/src/mihomo/clash_relay.rs` carry the regression tests for
the relay.

When appropriate:

```bash
cargo nextest run
cargo audit
cargo deny check
```

Do not suppress Clippy warnings without a concrete justification. When a lint is
disabled, the configuration states why — see `frontend/admin/.oxlintrc.json` for
the pattern.

### Verifying the whole thing

`cargo test` covers the Rust layers. Two things it cannot:

* **the interfaces in a browser.** The relay, the session cookie, and the CSP are
  exercised by opening the agent's own pages against a real kernel. The symptoms
  of getting these wrong are silent — a dashboard that renders and reports the
  backend as unreachable, a stream that opens and never speaks.
* **the field whitelist against a new kernel.** `mihomo -t` accepts unknown keys,
  so a new upstream release can add a field that L2 then reports as a typo.
  Regenerate with `scripts/gen-config-whitelist.py` when bumping the kernel.

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
serde_yaml
clap
ratatui
crossterm
tracing
tracing-subscriber
async-trait
thiserror
rusqlite          # bundled; see "Why rusqlite" below
```

**`rusqlite`, not `sqlx`.** Both were candidates; `rusqlite` was chosen with the
`bundled` feature, which compiles SQLite into the binary instead of linking the
distribution's library. Two reasons, and the second is the one that decided it:

* the agent targets Debian/Ubuntu *and* PVE LXC, where the system SQLite may be
  older than the schema features used, and a version mismatch is a startup
  failure rather than a degraded feature;
* `sqlx` is async-first, which for a local file with no network round trip buys
  nothing and costs a compile-time query verifier that needs a live database.

Consequences that follow from the choice: repository methods use a bounded
`SqlitePool` and run their statements through `spawn_blocking` where a call could
block, because `rusqlite` is synchronous. Schema changes are versioned by
`PRAGMA user_version`, not by migration files.

The exact crate versions must be chosen from current stable releases during
implementation; do not copy stale versions from this document.

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
