#!/usr/bin/env bash
#
# Installs proxyctl and its systemd unit.
#
# # What this does, and what it deliberately does not
#
# It installs the **agent**: the binary, the systemd unit, the configuration
# template, and the unprivileged user that runs it.
#
# It does **not** install the Mihomo kernel. That is a decision, not an omission:
#
# * the kernel is a separate program with its own licence (GPL-3.0), and having
#   an installer fetch it would put the corresponding-source obligation on this
#   script rather than on the operator's own `proxyctl mihomo update`;
# * the agent verifies the kernel against a checksum, and a release is the wrong
#   place for that decision — an operator may want a specific version, or none
#   yet, while they set up the agent first;
# * the agent works without a kernel. `proxyctl doctor` reports the environment,
#   the API comes up, and the interfaces serve. "No kernel yet" is a valid state
#   rather than a broken install.
#
# # Why it verifies a checksum
#
# The artifact is fetched over TLS from a URL that is itself derived from a
# release. TLS protects the transfer, not the artifact: whoever can publish a
# release can publish a binary, and a `.sha256` published in the same place is
# not independent evidence. It is still worth checking, because it catches a
# truncated download and a mismatched artifact name — the two failures that
# otherwise surface much later as a crash.
#
# # Usage
#
#   curl -fsSL https://raw.githubusercontent.com/ryuyb/proxyctl/main/scripts/install.sh | sudo bash
#
#   ./install.sh --version 0.1.0        # a specific release
#   ./install.sh --prefix /usr/local    # somewhere other than /usr
#   ./install.sh --dry-run              # print what would happen
#
# Environment:
#   PROXYCTL_VERSION    the release to install, instead of --version
#   PROXYCTL_BASE_URL   an alternative download root, for a mirrored release

set -euo pipefail

REPO="ryuyb/proxyctl"
BINARY="proxyctl"
PREFIX="/usr"
VERSION="${PROXYCTL_VERSION:-}"
BASE_URL="${PROXYCTL_BASE_URL:-https://github.com/${REPO}/releases/download}"
DRY_RUN=0
# Set when the script is piped to a shell, where `$0` is not a file on disk and
# so cannot be used to find a sibling checkout.
ASSUME_YES=0

# --- output ------------------------------------------------------------------
#
# Colour only when stderr is a terminal: a piped run should produce a clean log,
# and `curl | bash` always is one.

if [ -t 2 ]; then
  C_RESET=$'\033[0m'; C_BOLD=$'\033[1m'; C_DIM=$'\033[2m'
  C_RED=$'\033[31m'; C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'
else
  C_RESET=''; C_BOLD=''; C_DIM=''; C_RED=''; C_GREEN=''; C_YELLOW=''
fi

info()  { printf '%s\n' "$*" >&2; }
step()  { printf '%s==>%s %s\n' "$C_BOLD$C_GREEN" "$C_RESET" "$*" >&2; }
warn()  { printf '%s==>%s %s\n' "$C_BOLD$C_YELLOW" "$C_RESET" "$*" >&2; }
die()   { printf '%s==>%s %s\n' "$C_BOLD$C_RED" "$C_RESET" "$*" >&2; exit 1; }

usage() {
  # From a checkout the header is the documentation. Piped to `bash`, `$0` is not
  # the script, so `sed` would fail or read the wrong file — the fallback is not
  # as detailed, and says where to find the rest.
  if [ -f "${BASH_SOURCE[0]:-}" ]; then
    sed -n '3,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  else
    cat <<'TEXT'
Installs proxyctl and its systemd unit.

  --version <v>    the release to install (default: the latest)
  --prefix <dir>   install under this prefix (default: /usr)
  --base-url <u>   a different download root, for a mirrored release
  --dry-run        print what would happen, change nothing
  --yes, -y        do not prompt

The full explanation is in the header of scripts/install.sh, and in the README.
TEXT
  fi
  exit 0
}

# --- arguments ---------------------------------------------------------------

while [ $# -gt 0 ]; do
  case "$1" in
    --version)   shift; VERSION="${1:-}"; [ -n "$VERSION" ] || die "--version needs a value" ;;
    --prefix)    shift; PREFIX="${1:-}";  [ -n "$PREFIX" ]  || die "--prefix needs a value" ;;
    --base-url)  shift; BASE_URL="${1:-}"; [ -n "$BASE_URL" ] || die "--base-url needs a value" ;;
    --dry-run)   DRY_RUN=1 ;;
    --yes|-y)    ASSUME_YES=1 ;;
    -h|--help)   usage ;;
    *)           die "unknown argument: $1 (try --help)" ;;
  esac
  shift
done

# --- preconditions -----------------------------------------------------------

# Root, because every remaining step writes outside the home directory.
if [ "$(id -u)" -ne 0 ]; then
  die "this installer writes to ${PREFIX} and /etc/proxy-agent and must run as root.
     Re-run with: curl -fsSL <url> | sudo bash"
fi

for tool in curl tar sha256sum install; do
  command -v "$tool" >/dev/null 2>&1 || die "${tool} is required but not installed"
done

for dir in /etc/systemd/system /run/systemd/system; do
  if [ ! -d "$dir" ]; then
    warn "$dir is missing: this host does not look like it runs systemd."
    warn "The binary will be installed, but the unit cannot be enabled."
    warn "See the README for running it under another init."
    HAVE_SYSTEMD=0
    break
  fi
  HAVE_SYSTEMD=1
done

# --- architecture ------------------------------------------------------------

case "$(uname -m)" in
  x86_64|amd64)  ARCH="amd64" ;;
  aarch64|arm64) ARCH="arm64" ;;
  *) die "unsupported architecture: $(uname -m). Build from source — see the README." ;;
esac

case "$(uname -s)" in
  Linux) ;;
  *) die "this installer targets Linux; $(uname -s) is not supported. Build from source — see the README." ;;
esac

# --- resolve the version -----------------------------------------------------

if [ -z "$VERSION" ]; then
  step "Looking up the latest release"
  # `releases/latest` skips pre-releases, which is what we want: a pre-release is
  # not what an unattended installer should choose. The API is used rather than
  # following the `/releases/latest` redirect because the redirect target needs
  # parsing anyway, and the API gives an error body worth showing.
  api="https://api.github.com/repos/${REPO}/releases/latest"
  # The HTTP status is captured rather than relying on `-f`, because the two
  # failures below need different advice and `-f` collapses them into one exit
  # code.
  http="$(curl -sSL --max-time 30 -o /tmp/proxyctl-release.$$ -w '%{http_code}' "$api" 2>/dev/null)" \
    || http="000"

  case "$http" in
    200) json="$(cat /tmp/proxyctl-release.$$ 2>/dev/null || true)" ;;
    404)
      # `releases/latest` answers 404 in two different situations, and the two
      # need different advice:
      #
      #   * nothing has been released at all;
      #   * releases exist, but every one is marked a **pre-release**, which
      #     `latest` skips by definition.
      #
      # The second is the trap. It happened here: the only release was a
      # pre-release, so this path told a reader to build from source while a
      # working artifact sat one URL away. Looking for a pre-release before
      # giving up costs one request and turns a dead end into an install.
      rm -f /tmp/proxyctl-release.$$
      listing="$(curl -sSL --max-time 30 \
        "https://api.github.com/repos/${REPO}/releases?per_page=1" 2>/dev/null || true)"
      # A JSON array with a non-null `tag_name` means a release exists. Matched
      # with `sed` rather than parsed, because the only fields needed are the tag
      # and whether the array was empty.
      fallback="$(printf '%s' "$listing" \
        | sed -n 's/.*"tag_name" *: *"v\{0,1\}\([^"]*\)".*/\1/p' | head -1)"

      if [ -n "$fallback" ]; then
        warn "no *latest* release: every release is marked a pre-release."
        warn "Installing ${fallback} from the most recent one."
        warn "Pass --version to choose a different one, or check the release page:"
        warn "  https://github.com/${REPO}/releases"
        VERSION="$fallback"
        SKIP_LOOKUP=1
      else
        die "this repository has no published release yet.
     Build from source instead:
       git clone https://github.com/${REPO} && cd proxyctl
       cargo build --release -p proxyctl"
      fi ;;
    000)
      rm -f /tmp/proxyctl-release.$$
      die "could not reach the GitHub API.
     Check your network, pass --version to install a specific release, or set
     PROXYCTL_BASE_URL for a mirror." ;;
    *)
      rm -f /tmp/proxyctl-release.$$
      die "the GitHub API answered ${http}. Pass --version to install a specific
     release, or set PROXYCTL_BASE_URL for a mirror." ;;
  esac
  rm -f /tmp/proxyctl-release.$$

  # Set by the pre-release fallback above, which has already chosen a version.
  if [ "${SKIP_LOOKUP:-0}" -eq 0 ]; then
    VERSION="$(printf '%s' "$json" | sed -n 's/.*"tag_name" *: *"v\{0,1\}\([^"]*\)".*/\1/p' | head -1)"
    [ -n "$VERSION" ] || die "the latest release has no tag name; pass --version to name one."
  fi
fi

VERSION="${VERSION#v}"
ARTIFACT="${BINARY}-${VERSION}-linux-${ARCH}"
ARCHIVE="${ARTIFACT}.tar.gz"
URL="${BASE_URL}/v${VERSION}/${ARCHIVE}"

step "Installing ${BINARY} ${VERSION} (linux/${ARCH})"
info "  from     ${URL}"
info "  prefix   ${PREFIX}"
info "  unit     ${PREFIX}/lib/systemd/system/proxy-agent.service"

if [ "$DRY_RUN" -eq 1 ]; then
  info ""
  info "${C_DIM}--dry-run: nothing was changed.${C_RESET}"
  exit 0
fi

# --- the user ----------------------------------------------------------------

# Created before the files, because several of the steps below chown to it.
# `--system` gives no ageing and no shell prompt, which is what a service account
# should have: it is not a login.
if id "$BINARY" >/dev/null 2>&1 || id proxy-agent >/dev/null 2>&1; then
  step "The proxy-agent user already exists"
else
  step "Creating the proxy-agent user"
  if command -v useradd >/dev/null 2>&1; then
    useradd --system --no-create-home --shell /usr/sbin/nologin --user-group proxy-agent \
      || die "could not create the proxy-agent user"
  else
    die "useradd is missing; create the proxy-agent user and group by hand, then re-run"
  fi
fi
UNIT_USER=proxy-agent

# --- download and verify -----------------------------------------------------

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

step "Downloading"
curl -fsSL --max-time 300 -o "$TMP/$ARCHIVE" "$URL" || die \
  "could not download ${URL}
     If this is a private or unpublished release, build from source instead."

# The checksum is fetched from the same place as the artifact, so it is not
# independent evidence — it catches a truncated or misnamed download, not a
# substituted one. Recorded here so the next reader does not mistake it for more.
if curl -fsSL --max-time 60 -o "$TMP/${ARCHIVE}.sha256" "${URL}.sha256" 2>/dev/null; then
  step "Verifying the checksum"
  expected="$(awk '{print $1}' "$TMP/${ARCHIVE}.sha256" | head -1)"
  actual="$(sha256sum "$TMP/$ARCHIVE" | awk '{print $1}')"
  if [ "$expected" != "$actual" ]; then
    die "checksum mismatch — the download may be corrupt or tampered with
     expected ${expected}
     actual   ${actual}"
  fi
else
  warn "no checksum was published for this release; skipping verification"
fi

step "Unpacking"
tar xzf "$TMP/$ARCHIVE" -C "$TMP" --strip-components=1 \
  || die "the archive could not be unpacked"

[ -f "$TMP/$BINARY" ] || die "the archive has no ${BINARY} binary"
[ -x "$TMP/$BINARY" ] || die "the unpacked ${BINARY} is not executable"

# --- install -----------------------------------------------------------------

BIN_DIR="${PREFIX}/bin"
UNIT_DIR="${PREFIX}/lib/systemd/system"
# Matches `DEFAULT_KERNEL_BINARY` in crates/bootstrap/src/config/mod.rs, and the
# `ReadWritePaths` entry in the unit. All three have to agree.
KERNEL_DIR="${PREFIX}/lib/proxy-agent"
DOC_DIR="${PREFIX}/share/doc/proxy-agent"
CONFIG_DIR="/etc/proxy-agent"
CONFIG="${CONFIG_DIR}/config.toml"

step "Installing the binary"
install -d -m 0755 "$BIN_DIR"
# Unconditionally overwritten. The binary is not user state, and leaving an old
# one in place is how "I upgraded and nothing changed" happens.
install -m 0755 "$TMP/$BINARY" "${BIN_DIR}/${BINARY}"

# Only when it actually differs: replacing it rewrites `Documentation=` in the
# unit, and a needless write bumps the mtime that systemd uses to decide whether
# a daemon-reload is needed.
if [ -f "$TMP/proxy-agent.service" ]; then
  step "Installing the systemd unit"
  install -d -m 0755 "$UNIT_DIR"
  if ! cmp -s "$TMP/proxy-agent.service" "${UNIT_DIR}/proxy-agent.service"; then
    # `Documentation=` in the unit points at a path under `${PREFIX}`, so a
    # non-default prefix has to be reflected or `systemd-analyze verify` reports a
    # missing file.
    sed "s|/usr/share/doc/proxy-agent|${DOC_DIR}|g" "$TMP/proxy-agent.service" \
      > "${UNIT_DIR}/proxy-agent.service"
    UNIT_CHANGED=1
  else
    UNIT_CHANGED=0
  fi
fi

if [ -f "$TMP/config.toml.example" ]; then
  step "Installing the configuration example"
  install -d -m 0755 "$DOC_DIR"
  install -m 0644 "$TMP/config.toml.example" "${DOC_DIR}/config.toml.example"
fi

if [ -f "$TMP/METACUBEXD_VERSION" ]; then
  install -m 0644 "$TMP/METACUBEXD_VERSION" "${DOC_DIR}/METACUBEXD_VERSION"
fi

# --- the kernel's install directory ------------------------------------------

# `kernel.binary` defaults to `/usr/lib/proxy-agent/mihomo`, and the agent writes
# it there when installing or upgrading a kernel. The unit lists that directory in
# `ReadWritePaths` (the alternative is `ProtectSystem=strict` making it read-only
# and every kernel install failing), and it has to exist with the right owner
# before the first start — `ReadWritePaths` re-opens an existing path, it does not
# create one.
#
# The kernel itself is *not* downloaded here: see the header for why.
step "Preparing the kernel directory"
install -d -m 0755 "$KERNEL_DIR"
chown "${UNIT_USER}:${UNIT_USER}" "$KERNEL_DIR" 2>/dev/null \
  || warn "could not set the owner of ${KERNEL_DIR} to ${UNIT_USER}"

# --- the configuration -------------------------------------------------------

# The configuration directory must be traversable by the service user.
#
# `0700 root:root` looks like the stricter choice and is not: the unit runs as
# `proxy-agent`, so a directory only root can enter makes the file inside it
# unreadable no matter who owns the file. The mode stays 0700 — owner-only — but
# the owner has to be the service user, which is also what the unit's
# `ConfigurationDirectoryMode=0700` implies when systemd creates the directory
# itself. Found by installing and starting the unit.
own_config_dir() {
  if [ -d "$CONFIG_DIR" ]; then
    chown "${UNIT_USER}:${UNIT_USER}" "$CONFIG_DIR" 2>/dev/null \
      || warn "could not set the owner of ${CONFIG_DIR} to ${UNIT_USER}"
    chmod 0700 "$CONFIG_DIR"
  fi
}

# Directory modes come from the unit (`ConfigurationDirectoryMode=0700`), but the
# file is created here so a first run has something to read. `0600` is not
# cosmetic: the loader refuses to start on a group- or world-readable file,
# because it may hold the kernel secret.
#
# **Owned by the service user, not root.** Mode 0600 with a root owner is a file
# the agent cannot read — the unit runs as `proxy-agent`, so the service fails at
# startup with `Permission denied` and systemd reports only an exit code. Found
# by installing and starting the unit rather than by reading this script.
if [ -f "$CONFIG" ]; then
  step "Keeping the existing ${CONFIG}"
  # An upgrade must not discard an operator's configuration — including a bad
  # one. The loader's refusal is the right place for that judgement, and it names
  # the file.
  #
  # Its ownership is corrected, though: an existing file may predate the service
  # user, or have been created by an operator's editor running as root.
  chown "${UNIT_USER}:${UNIT_USER}" "$CONFIG" 2>/dev/null \
    || warn "could not set the owner of ${CONFIG} to ${UNIT_USER}"
  chmod 0600 "$CONFIG"
  own_config_dir
else
  step "Writing ${CONFIG}"
  install -d -m 0700 "$CONFIG_DIR"
  cat > "$CONFIG" <<'TOML'
# proxyctl configuration.
#
# Every section and field is optional; an empty file is valid. See
# /usr/share/doc/proxy-agent/config.toml.example for the complete reference,
# and run `proxyctl agent run --print-config` to see what is in effect and
# which source supplied each value.
#
# This file must stay at mode 0600. It may hold the kernel secret, and the
# loader refuses to start on a group- or world-readable file.

[agent]
socket = "/run/proxy-agent/agent.sock"

[controller]
# A unix socket by default: it cannot be reached from off-host, so it cannot be
# exposed by accident. The kernel's own field for this is
# `external-controller-unix` — `external-controller` takes a host:port and
# silently ignores a path, which leaves the proxy working and the control API
# missing.
endpoint = "/run/proxy-agent/mihomo.sock"
TOML
  chmod 0600 "$CONFIG"
  chown "${UNIT_USER}:${UNIT_USER}" "$CONFIG" 2>/dev/null \
    || warn "could not set the owner of ${CONFIG} to ${UNIT_USER}"
  own_config_dir
fi

# --- systemd -----------------------------------------------------------------

UNIT_ENABLED=0
if [ "${HAVE_SYSTEMD:-1}" -eq 1 ]; then
  step "Reloading systemd"
  systemctl daemon-reload || warn "systemctl daemon-reload failed"

  # Enabled, not started. Starting is the operator's call: this agent manages a
  # kernel, and bringing it up unasked on someone's server is not the installer's
  # decision to make.
  #
  # A failure here is normal for a non-default prefix: `systemctl enable` looks
  # the unit up under `/etc/systemd/system` and `/usr/lib/systemd/system`, so a
  # unit installed to `/usr/local/lib/systemd/system` is invisible to it. The
  # summary reports what actually happened rather than what was attempted.
  step "Enabling the unit (not starting it)"
  if systemctl enable proxy-agent >/dev/null 2>&1; then
    UNIT_ENABLED=1
  else
    warn "systemctl could not enable the unit."
    if [ "$PREFIX" != "/usr" ]; then
      warn "This is expected for --prefix ${PREFIX}: systemd looks for units under"
      warn "/etc/systemd/system and /usr/lib/systemd/system. Link it yourself with:"
      warn "  ln -s ${UNIT_DIR}/proxy-agent.service /etc/systemd/system/proxy-agent.service"
      warn "  systemctl daemon-reload && systemctl enable proxy-agent"
    else
      warn "Start it manually with: systemctl start proxy-agent"
    fi
  fi
fi

# --- summary -----------------------------------------------------------------

info ""
step "Installed ${BINARY} ${VERSION}"
info ""
info "  binary   ${BIN_DIR}/${BINARY}"
info "  config   ${CONFIG}"
if [ "${HAVE_SYSTEMD:-1}" -eq 1 ]; then
  if [ "${UNIT_ENABLED:-0}" -eq 1 ]; then
    info "  unit     ${UNIT_DIR}/proxy-agent.service (enabled, not started)"
  else
    info "  unit     ${UNIT_DIR}/proxy-agent.service (NOT enabled — see above)"
  fi
fi
info ""
info "${C_BOLD}Next:${C_RESET}"
info ""
info "  1. Review ${CONFIG}, then start the agent:"
info "       ${C_DIM}systemctl start proxy-agent${C_RESET}"
info "     Do not run \`sudo proxyctl agent run\` instead. It creates the socket as"
info "     root, leaves it unmanageable by systemd, and refuses the very user the"
info "     service runs as — so every later command fails while an agent is up."
info ""
info "  2. Install a Mihomo kernel — this installer deliberately does not."
info "     The version is required: the agent verifies the checksum, and picking"
info "     one is a decision rather than a default. Any release tag will do."
info "       ${C_DIM}proxyctl mihomo update v1.19.30${C_RESET}"
info ""
info "  3. Check the environment before trusting it:"
info "       ${C_DIM}proxyctl doctor${C_RESET}"
info ""
info "  4. The web interface needs a session, which needs a token:"
info "       ${C_DIM}proxyctl token issue --principal admin${C_RESET}"
info "     To reach it from another machine, set [api] bind in the config."
info "     Over TCP a token is required — there is no loopback exemption."
info ""
