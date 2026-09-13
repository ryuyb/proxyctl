#!/usr/bin/env bash
#
# Fetches the metacubexd dashboard artifact into `frontend/metacubexd/dist`.
#
# # Why the published artifact rather than a build
#
# The agent embeds a static dashboard. Building it here would make the Rust build
# depend on a Node toolchain it otherwise does not need, and the upstream build
# fetches Google Fonts — so an offline or sandboxed `cargo build` would fail for
# a reason that has nothing to do with the agent.
#
# Upstream already publishes exactly what we need: its `gh-pages` branch is the
# static artifact, produced by upstream's own release workflow. We consume that
# rather than forking the UI, which is also what keeps this a packaging step
# instead of a maintained patch set against someone else's Nuxt app.
#
# # What it writes
#
#   frontend/metacubexd/dist/            the artifact, served at `/ui`
#   frontend/metacubexd/UPSTREAM_VERSION the tag it came from, e.g. `v1.273.1`
#
# The version file is the answer to "which dashboard is in this binary". The
# bundle carries `appVersion` too, and this script asserts the two agree — a
# mismatch would mean the branch moved between the tag lookup and the download,
# and a silent mismatch is worse than a failed build.
#
# # Which version is fetched
#
# `frontend/metacubexd/UPSTREAM_VERSION` is committed, so it is the default and
# the source of truth: a checkout builds against the version it recorded, and CI
# needs no network round trip to decide what to download. `--version` overrides
# it for a one-off, and `--latest` asks upstream for the newest release.
#
# The distinction matters for reproducibility. Resolving "latest" on every run
# means two builds of the same commit can embed different dashboards, and that a
# GitHub API rate limit — which is per source address, and a CI runner shares
# one — turns into a failed build rather than a pinned one.
#
# # Failure behaviour
#
# A missing dashboard is not a build failure. The agent serves a "not deployed"
# page instead, and `--print-config`-style startup output says so. A developer
# without network, or a backend-only checkout, must not be blocked by a UI they
# are not touching. Pass `--strict` to make a fetch failure an error, which is
# what a release build should do.
#
# Usage:
#   scripts/fetch-metacubexd.sh              # the committed version; warn on failure
#   scripts/fetch-metacubexd.sh --strict     # fail if the artifact is missing
#   scripts/fetch-metacubexd.sh --version v1.273.1
#   scripts/fetch-metacubexd.sh --latest     # resolve the newest upstream release

set -euo pipefail

REPO="MetaCubeX/metacubexd"
DEST="frontend/metacubexd/dist"
VERSION_FILE="frontend/metacubexd/UPSTREAM_VERSION"
METADATA="frontend/metacubexd/artifact.toml"

STRICT=0
VERSION=""
LATEST=0
while [ $# -gt 0 ]; do
  case "$1" in
    --strict) STRICT=1 ;;
    --latest) LATEST=1 ;;
    --version)
      shift
      VERSION="${1:-}"
      if [ -z "$VERSION" ]; then
        echo "fetch-metacubexd: --version needs a value" >&2
        exit 2
      fi
      ;;
    -h|--help)
      sed -n '3,40p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "fetch-metacubexd: unknown argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# A missing `curl` is only fatal under `--strict`. The script exists to *enable*
# an optional feature, so its own absence must not become a new hard requirement.
if ! command -v curl >/dev/null 2>&1; then
  echo "fetch-metacubexd: curl is not available" >&2
  [ "$STRICT" -eq 1 ] && exit 1 || exit 0
fi

die_or_warn() {
  if [ "$STRICT" -eq 1 ]; then
    echo "fetch-metacubexd: $1" >&2
    exit 1
  fi
  echo "fetch-metacubexd: $1 (the dashboard will not be embedded)" >&2
  exit 0
}

# Resolves the tag to fetch, in order of precedence: `--version`, then the
# committed `UPSTREAM_VERSION`, then `--latest` (which is the only path that
# touches the network). Without `--latest` this is a pure download of a pinned
# tag, so the same commit always embeds the same dashboard.
#
# # Why this asks for the HTTP status separately
#
# It used to pipe `curl -sSL` straight into `sed`, with `|| true` on the end, and
# that combination failed in CI while passing locally. The unauthenticated GitHub
# API rate-limits by source address, and a runner is a shared address: the answer
# becomes `403` with a JSON *error* body, which has no `tag_name`, so `sed`
# produced an empty string. `|| true` then swallowed the failure, `--max-time`
# never fired, and the script reported "could not determine an upstream version"
# — a message about a missing version rather than about being rate-limited.
#
# So the status is asked for explicitly and checked: a non-2xx answer is retried
# (a rate limit is temporary), and `latest` is allowed to have no *non-prerelease*
# release by falling back to the newest one of any kind.
api_get() {
  curl -sS --max-time 30 --retry 3 --retry-delay 2 --retry-all-errors \
    -H 'Accept: application/vnd.github+json' \
    -w '\n%{http_code}' "$1" 2>/dev/null
}

# The committed version, when no explicit one was given and `--latest` was not
# asked for. Reading it here is what makes the fetch reproducible and keeps CI off
# the network for a value already in the tree.
if [ -z "$VERSION" ] && [ "$LATEST" -eq 0 ] && [ -s "$VERSION_FILE" ]; then
  VERSION="$(sed -n '1{/^[[:space:]]*$/d;p;q;}' "$VERSION_FILE")"
  [ -n "$VERSION" ] && \
    echo "fetch-metacubexd: using the committed ${VERSION} (pass --latest to update)" >&2
fi

if [ -z "$VERSION" ] && [ "$LATEST" -eq 1 ]; then
  # The body and the status arrive together; the code is the last line.
  RESPONSE="$(api_get "https://api.github.com/repos/${REPO}/releases/latest")" || RESPONSE=""

  if [ -n "$RESPONSE" ]; then
    HTTP="$(printf '%s' "$RESPONSE" | tail -n 1)"
  else
    HTTP="000"
  fi
  # Everything but the status line.
  BODY="$(printf '%s' "$RESPONSE" | sed '$d')"

  if [ "$HTTP" = "200" ]; then
    VERSION="$(printf '%s' "$BODY" \
      | sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -1)"
  fi

  # `releases/latest` skips pre-releases. If upstream's newest release is one,
  # the endpoint 404s while a perfectly usable tag exists, so ask for the list
  # and take the newest of any kind.
  if [ -z "$VERSION" ]; then
    LISTING="$(api_get "https://api.github.com/repos/${REPO}/releases?per_page=1")" || LISTING=""
    if [ -n "$LISTING" ] && [ "$(printf '%s' "$LISTING" | tail -n 1)" = "200" ]; then
      VERSION="$(printf '%s' "$LISTING" | sed '$d' \
        | sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -1)"
      [ -n "$VERSION" ] && \
        echo "fetch-metacubexd: no non-prerelease release; using ${VERSION}" >&2
    fi
  fi

  if [ -z "$VERSION" ]; then
    die_or_warn "could not determine an upstream version (the GitHub API answered ${HTTP}; \
pass --version to pin one explicitly)"
  fi
fi

# Neither the file nor `--latest` produced a version: the file is missing or empty.
if [ -z "$VERSION" ]; then
  die_or_warn "could not determine an upstream version (${VERSION_FILE} is missing or empty; \
pass --version, or --latest to ask upstream)"
fi

# A pinned version may be written with or without the leading `v`; upstream tags
# use `v`. Normalising here means the version file always holds the tag as it
# exists upstream, so it can be compared against `git ls-remote` by hand.
case "$VERSION" in
  "") die_or_warn "could not determine an upstream version" ;;
  v*) ;;
  *) VERSION="v${VERSION}" ;;
esac

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# The `gh-pages` branch, not the release tarball: the release carries the source,
# and what we want is the built artifact. Upstream's `release.yml` pushes the
# artifact there, so it is the published form of exactly this directory.
URL="https://github.com/${REPO}/archive/refs/heads/gh-pages.tar.gz"
if ! curl -sSL --max-time 300 -o "$TMP/gh-pages.tar.gz" "$URL"; then
  die_or_warn "could not download ${URL}"
fi

mkdir -p "$TMP/unpacked"
# `--strip-components=1` drops the `metacubexd-gh-pages/` prefix that GitHub
# adds to an archive of a branch.
if ! tar xzf "$TMP/gh-pages.tar.gz" -C "$TMP/unpacked" --strip-components=1 \
    2>/dev/null; then
  die_or_warn "the downloaded archive could not be unpacked"
fi

if [ ! -f "$TMP/unpacked/index.html" ]; then
  die_or_warn "the archive has no index.html, so it is not a dashboard artifact"
fi

# The artifact must be relocatable under `/ui`. Upstream builds `gh-pages` with a
# relative `baseURL`, which is what makes that work; an absolute one would make
# every asset 404 behind our prefix. This is asserted rather than assumed,
# because the symptom — a blank page with 404s — points at our routing rather
# than at upstream's build flags.
if grep -q 'src="/_nuxt/' "$TMP/unpacked/index.html"; then
  die_or_warn "the artifact uses absolute asset paths and cannot be served under /ui"
fi

# The artifact's own version, read back from the bundle. It is embedded in
# `window.__NUXT__` as `appVersion`.
ARTIFACT_VERSION="$(sed -n 's/.*appVersion:"\([^"]*\)".*/\1/p' "$TMP/unpacked/index.html" | head -1)"
if [ -z "$ARTIFACT_VERSION" ]; then
  die_or_warn "could not read appVersion from the artifact"
fi

# The tag and the artifact must agree. They can diverge when the branch moves
# between the version lookup and the download, and a binary silently carrying a
# dashboard other than the one recorded is exactly the drift this guards.
EXPECTED="${VERSION#v}"
if [ "$ARTIFACT_VERSION" != "$EXPECTED" ]; then
  echo "fetch-metacubexd: the artifact is ${ARTIFACT_VERSION} but the tag is ${VERSION}" >&2
  echo "fetch-metacubexd: the gh-pages branch moved during the download; retry" >&2
  [ "$STRICT" -eq 1 ] && exit 1 || exit 0
fi

# Replaced rather than merged: a stale file from an older build would be served
# from a path the new build no longer references, which is invisible until
# someone has it cached.
rm -rf "$DEST"
mkdir -p "$(dirname "$DEST")"
mv "$TMP/unpacked" "$DEST"

FILE_COUNT="$(find "$DEST" -type f | wc -l | tr -d ' ')"
SIZE="$(du -sh "$DEST" | cut -f1 | tr -d ' ')"

# The version is written to a file that *is* tracked, so a checkout states which
# dashboard it carries even before the artifact is fetched, and a diff shows an
# upgrade as a one-line change rather than 160 new hashed filenames.
printf '%s\n' "$VERSION" > "$VERSION_FILE"

# A machine-readable record, so the build can report what is embedded without
# re-deriving it. Kept beside the version file rather than inside the artifact,
# because the artifact is replaced wholesale on each fetch.
cat > "$METADATA" <<TOML
# @generated by scripts/fetch-metacubexd.sh — do not edit.
#
# What was embedded, and where it came from. Read by \`build.rs\` so the agent can
# report the dashboard's version, and by a reviewer asking "what is in this
# binary" without having to unpack it.
version = "${VERSION}"
app_version = "${ARTIFACT_VERSION}"
source = "https://github.com/${REPO}/tree/gh-pages"
files = ${FILE_COUNT}
size = "${SIZE}"
TOML

echo "fetch-metacubexd: ${VERSION} → ${DEST} (${FILE_COUNT} files, ${SIZE})"
