<div align="center">

# proxyctl

**A Linux-native control plane for the Mihomo proxy kernel.**

Lifecycle, versioned configuration with rollback, subscriptions, capability
detection — plus a web interface and a terminal one.

[**English**](README.md) · [**中文**](README.zh-CN.md)

[Install](#install) · [Quick start](#quick-start) · [CLI](#cli) · [Web interface](#web-interface) · [How it works](#how-it-works)

</div>

---

## What this is

Mihomo is an excellent proxy kernel with a capable control API. What it does not
have is *operations*: an immutable history of configurations, a way back from a
bad one, subscription scheduling that cannot destroy a working setup, and an
honest answer to "what can this container actually do".

`proxyctl` is that layer. It is a single binary with two roles — a client and an
agent — and it refuses to guess about the environment it runs in.

```text
                     ┌─────────────────────────────┐
   browser ─────────►│  proxy-agent                │
                     │  ├── /          operator UI │
                     │  ├── /ui        dashboard   │
                     │  ├── /clash-api kernel feed │
                     │  └── /api/v1    REST        │
                     └──────────┬──────────────────┘
                                │ unix socket
                     ┌──────────▼──────────────────┐
   clients ─────────►│  proxyctl (same binary)     │
                     └─────────────────────────────┘
                                │
                     ┌──────────▼──────────────────┐
                     │  mihomo (the proxy itself)  │
                     └─────────────────────────────┘
```

**It is not** a replacement for Mihomo, a complete Sub-Store, or a dashboard.
Where an upstream project already does something well, this uses it rather than
reimplementing it — the dashboard is upstream's, embedded.

### What it does that matters

| | |
|---|---|
| **Configuration versions** | Every config is an immutable version with a checksum. `list`, `validate`, `activate`, `rollback` — and the active file is never edited in place. |
| **Safe failures** | A failed subscription update, conversion, validation, or reload leaves the previous working configuration active. This is the invariant everything else is built around. |
| **Honest capabilities** | TUN, nftables, and policy routing are reported as one of five states — `Supported`, `Unsupported`, `Unavailable`, `Misconfigured`, `Unknown` — with the probe and what it saw. Never a boolean. |
| **Two interfaces** | An operator interface for lifecycle and configuration, and the upstream dashboard for proxy groups, latency, and live traffic. |
| **One binary** | `proxyctl` is the client and the agent. No second program to install, no version skew between them. |

---

## Install

### One command

```bash
curl -fsSL https://raw.githubusercontent.com/ryuyb/proxyctl/main/scripts/install.sh | sudo bash
```

That installs the binary, the systemd unit, a configuration file, and the
unprivileged `proxy-agent` user the service runs as. It **enables** the unit and
does not start it — bringing up a proxy kernel on someone's server is not the
installer's decision to make.

<details>
<summary>What the script does, and what it deliberately does not</summary>

**Install to a prefix other than `/usr`:**

```bash
curl -fsSL <url> | sudo bash -s -- --prefix /usr/local
```

Note that systemd only looks for units under `/etc/systemd/system` and
`/usr/lib/systemd/system`, so a non-default prefix needs a symlink. The script
says so when `systemctl enable` fails.

**Pin a version, or use a mirror:**

```bash
./scripts/install.sh --version 0.1.0
PROXYCTL_BASE_URL=https://mirror.example/proxyctl ./scripts/install.sh
./scripts/install.sh --dry-run      # print what would happen
```

**It does not install the Mihomo kernel.** That is a decision:

* the kernel is a separate program under its own licence, and having an installer
  fetch it would move the corresponding-source obligation onto this script;
* the agent verifies the kernel against a checksum, and choosing a version is the
  operator's call;
* the agent works without one. `proxyctl doctor` and the web interface both come
  up, and "no kernel yet" is a valid state rather than a broken install.

Install one with `proxyctl mihomo install`.

**On checksums.** The `.sha256` is fetched from the same place as the artifact, so
it is not independent evidence — it catches a truncated or mismatched download,
not a substituted one. The script says so when it verifies.
</details>

### From source

```bash
git clone https://github.com/ryuyb/proxyctl
cd proxyctl
cargo build --release -p proxyctl     # binary at target/release/proxyctl
sudo install -m 0755 target/release/proxyctl /usr/bin/proxyctl
sudo install -d -o proxy-agent -g proxy-agent -m 0755 /usr/lib/proxy-agent
sudo install -m 0644 packaging/systemd/proxy-agent.service /usr/lib/systemd/system/
```

Or use `scripts/install.sh` from the checkout, which does all of that and more.

### Requirements

Linux, systemd, and either `x86_64` or `aarch64`. Debian/Ubuntu and PVE LXC are
what has been tested; other distributions should work but have not been verified.

The prebuilt binary has no runtime dependencies. Building from source needs Rust
1.85 or newer.

---

## Quick start

```bash
# 1. Start the agent
sudo systemctl start proxy-agent

# 2. Install a kernel
sudo -u proxy-agent proxyctl mihomo install

# 3. See what this environment can actually do
sudo -u proxy-agent proxyctl doctor

# 4. Start the kernel
sudo -u proxy-agent proxyctl start
```

### Why `sudo -u proxy-agent`

The agent socket is `0660` inside a `0750` directory, owned by `proxy-agent`. That
is the access-control boundary: whoever can open the socket can manage the kernel.
Your own user is not in that group by default, so either prefix commands with
`sudo -u proxy-agent`, or add yourself once:

```bash
sudo usermod -aG proxy-agent "$USER"    # then log out and back in
```

To reach the web interface from another machine, set the listener in the
configuration:

```toml
# /etc/proxy-agent/config.toml
[api]
bind = "0.0.0.0:9090"
```

Over TCP a token is required, with **no loopback exemption** — see
[Security](#security). Issue one first, or the agent refuses to start.

---

## CLI

```bash
proxyctl status                 # what the kernel is doing
proxyctl doctor                 # environment and capabilities
proxyctl start | stop | restart | reload

proxyctl mihomo install         # fetch and verify a kernel release
proxyctl mihomo version

proxyctl config list            # every configuration version
proxyctl config validate FILE   # preflight, syntax, semantic
proxyctl config activate ID
proxyctl config rollback ID     # back to a version that worked

proxyctl subscription list
proxyctl subscription update NAME

proxyctl connections            # live connections, with process details for admins
proxyctl logs -f                # follow the kernel's log
proxyctl jobs                   # what has run recently
proxyctl audit                  # who did what
proxyctl tui                    # the terminal interface
```

Every command accepts `--json` for scripting, and `--socket PATH` or
`--token TOKEN` to reach a different agent.

### Exit codes

A contract, because these are used in scripts:

| Code | Meaning |
|---|---|
| `0` | success |
| `1` | failure |
| `2` | usage error |
| `3` | not found |
| `4` | conflicts with current state |
| `5` | not permitted |
| `6` | a dependency was unreachable (the kernel, a subscription source) |
| `7` | not implemented |

Note that `config validate` exits non-zero for a document the agent *rejects*,
even though the request itself succeeded with `200`. A `200` can still be bad news.

---

## Web interface

Two interfaces, served by the agent on the same origin.

**The operator interface** (`/`) covers what this project owns: lifecycle,
configuration versions and rollback, subscriptions, capabilities, doctor, recent
jobs, the audit trail, and live connections. It needs a session, which needs a
token:

```bash
sudo -u proxy-agent proxyctl token issue --principal admin --role admin
```

**The dashboard** (`/ui`) is [metacubexd](https://github.com/MetaCubeX/metacubexd),
upstream's own project, embedded as a built artifact. It covers proxy-group
switching, per-node latency tests, the traffic graph, and rule inspection — the
things a dashboard is for, which this project deliberately did not rebuild.

Its data comes from `/clash-api`, a same-origin relay to the kernel. That relay is
why the browser never holds the kernel's secret, and why the kernel's controller
can stay on a unix socket that nothing else can reach.

Both are embedded in the binary. A checkout without them still builds and serves a
page explaining how to fetch them.

---

## Configuration

One file, entirely optional — an empty file is valid and yields the defaults.

```text
Location   /etc/proxy-agent/config.toml
Mode       0600, enforced. The loader refuses to start otherwise: this file may
           hold the kernel secret.
Reference  /usr/share/doc/proxy-agent/config.toml.example
```

Values are resolved highest-first: **a command-line flag** → **an environment
variable** (`PROXYCTL_*`) → **this file** → **the built-in default**. To see what
is actually in effect and which source supplied each value:

```bash
sudo -u proxy-agent proxyctl agent run --print-config
```

Every path is configurable, and nothing hard-codes the packaged defaults. The
three directories come from `[paths]`, and the socket from `[agent] socket`.

---

## Security

The short version: the socket's permissions are the boundary, TCP requires a
token, and the browser gets a cookie rather than a credential.

* **Unix socket.** `0660` in a `0750` directory. The peer credential
  (`SO_PEERCRED`) is an *optional second* check, off by default because LXC uid
  mapping can make a correct peer look wrong.
* **TCP.** A token is required, with **no loopback exemption** — loopback is not a
  trust boundary on a host that also runs untrusted software. Configuring a
  listener requires at least one token to exist, or the agent refuses to start.
* **Browsers** exchange a token for an `HttpOnly; SameSite=Strict` cookie. There
  is deliberately no CSRF token: one would have to be readable by a script, which
  is what the cookie design exists to avoid.
* **The kernel's secret never reaches a page.** The relay injects it. Upstream's
  own all-in-one server puts its control token in the browser instead; this does
  not.
* **The kernel controller** is never bound to a public interface by default.

Full reasoning is in [`docs/adr/`](docs/adr/) and in the
[`AGENTS.md`](AGENTS.md) security section.

---

## How it works

```text
crates/
├── domain/          pure rules — no I/O, no runtime, no database
├── application/     use cases and Ports
├── infrastructure/  the adapters: SQLite, mihomo, systemd, the Clash API relay
├── interfaces/      the HTTP API and the two embedded interfaces
├── cli/             proxyctl: client commands, TUI, and the agent role
└── bootstrap/       composition root
```

Six crates and one binary. Dependencies point inward and an architecture test
enforces it — the domain knows nothing about Linux, HTTP, or SQLite, which is what
makes it testable without any of them.

**Where the state lives:**

```text
/etc/proxy-agent/config.toml        configuration        (0600)
/var/lib/proxy-agent/configs/       immutable versions
/var/lib/proxy-agent/database.sqlite
/run/proxy-agent/agent.sock         the agent        (0750 dir, 0660 socket)
/run/proxy-agent/mihomo.sock        the kernel
```

**Documentation at this stage** — the reasoning, the measurements, and the
questions that are still open rather than quietly assumed:

| | |
|---|---|
| [`docs/design/`](docs/design/) | per-feature design notes, including what measurement disproved |
| [`docs/adr/`](docs/adr/) | architecture decisions and their consequences |
| [`docs/research/`](docs/research/) | Phase 0: upstream behaviour, licences, and the security model |
| [`open-questions.md`](docs/research/open-questions.md) | what is unresolved, with evidence — not a wish list |

---

## Status

**Pre-release, in active development.** Working today and verified on a real
kernel: kernel lifecycle, configuration versioning and rollback, subscriptions,
Doctor, the REST API, the CLI, the TUI, both web interfaces, and the systemd
packaging with the one-command installer.

Not yet done:

* a `.deb`, so installation is a package rather than a script;
* subscription conversion through Sub-Store (the port exists; a native converter
  is what is wired up);
* anything multi-node.

Known open items with their evidence are in
[`open-questions.md`](docs/research/open-questions.md). Tests and gates:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
pnpm --dir frontend/admin check     # the front end is a separate toolchain
```

---

## Licence

The agent is dual-licensed under **MIT or Apache-2.0**, at your option — see
[`LICENSE-MIT`](LICENSE-MIT) and [`LICENSE-APACHE`](LICENSE-APACHE).

The Mihomo kernel is GPL-3.0 and is a separate program: this project does not
distribute it, and installs it at the operator's request. The embedded dashboard
is MIT, and carries upstream's own font and graphic attribution requirements —
see [`docs/research/13-licenses.md`](docs/research/13-licenses.md).
