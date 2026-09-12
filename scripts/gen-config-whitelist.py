#!/usr/bin/env python3
"""Generate the mihomo configuration field whitelist from upstream config.go.

Level 2 validation cannot rely on `mihomo -t` to catch a mistyped field: it
accepts unknown fields silently and the kernel then falls back to defaults, so a
config does not do what its author wrote. The whitelist closes that gap.

The list is derived from upstream's Go struct yaml tags rather than written by
hand, because a hand-written list drifts from upstream in exactly the way this
check exists to detect.

Usage:
    curl -sL https://raw.githubusercontent.com/MetaCubeX/mihomo/<TAG>/config/config.go -o /tmp/config.go
    python3 scripts/gen-config-whitelist.py /tmp/config.go <TAG> \
        > crates/infrastructure/src/validation/whitelist.rs
"""

import re
import sys

# Sections whose nested keys are checked. A typo inside `dns:` is as harmful as a
# typo at the top level, so the important nested mappings are covered too.
CHECKED_SECTIONS = {
    "dns": "RawDNS",
    "tun": "RawTun",
    "ntp": "RawNTP",
    "sniffer": "RawSniffer",
    "experimental": "RawExperimental",
    "profile": "RawProfile",
    "tls": "RawTLS",
    "external-controller-cors": "RawCors",
    "geox-url": "RawGeoXUrl",
    "iptables": "RawIPTables",
    "tuic-server": "RawTuicServer",
}


def structs(src: str) -> dict[str, list[str]]:
    found: dict[str, list[str]] = {}
    for m in re.finditer(r"type (\w+) struct \{(.*?)\n\}", src, re.S):
        name, body = m.group(1), m.group(2)
        tags = re.findall(r'yaml:"([^",]+)', body)
        if tags:
            found[name] = tags
    return found


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    path, tag = sys.argv[1], sys.argv[2]
    src = open(path, encoding="utf-8").read()
    found = structs(src)

    top = found.get("RawConfig")
    if not top:
        print("RawConfig not found in the supplied source", file=sys.stderr)
        return 1

    lines = [
        "//! The mihomo configuration field whitelist.",
        "//!",
        "//! **Generated file -- do not edit by hand.**",
        "//!",
        f"//! Source: `MetaCubeX/mihomo` tag `{tag}`, file `config/config.go`.",
        "//! Regenerate with `scripts/gen-config-whitelist.py` (see its docstring).",
        "//!",
        "//! # Why this exists",
        "//!",
        "//! `mihomo -t` accepts unknown fields without complaint, so a mistyped key",
        "//! (`mixed-portt` for `mixed-port`) passes validation and the kernel then",
        "//! falls back to its default. The config silently does not do what its author",
        "//! wrote, which is the failure this list exists to catch.",
        "//!",
        "//! # Treating a miss as a warning, not a rejection",
        "//!",
        "//! Upstream adds fields over time. Rejecting an unknown key outright would",
        "//! break a legitimate new config on an older agent, so this list is used to",
        "//! *report* unknown keys. The caller decides whether that is fatal; see",
        "//! `ConfigValidator::validate_semantic`.",
        "",
        f"/// The upstream tag this list was generated from.",
        f'pub const SOURCE_TAG: &str = "{tag}";',
        "",
        "/// Top-level configuration keys.",
        f"pub const TOP_LEVEL: &[&str] = &[",
    ]
    lines += [f'    "{t}",' for t in top]
    lines += ["];", "", "/// Keys checked within a nested section.", "pub const SECTIONS: &[(&str, &[&str])] = &["]
    for section, struct in CHECKED_SECTIONS.items():
        keys = found.get(struct)
        if not keys:
            continue
        lines.append(f'    ("{section}", &[')
        lines += [f'        "{k}",' for k in keys]
        lines.append("    ]),")
    lines += ["];", ""]

    sys.stdout.write("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
