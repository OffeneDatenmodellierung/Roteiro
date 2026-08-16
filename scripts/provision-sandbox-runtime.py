#!/usr/bin/env python3
"""Provision the pinned sandbox runtime so `--features exec-boxlite` can build.

This is the CI bootstrap for the chicken-and-egg in Stage 24: the documented way
to obtain the runtime is `roteiro security prefetch --allow-download`, but
building `roteiro` with `--all-features` is what needs the runtime in the first
place. Rather than build the binary twice, CI runs this.

**It reads the digests out of `crates/rto-exec/src/runtime_pins.rs` itself**, so
it cannot drift from what the build script will verify a moment later. If the pin
moves, this follows automatically; if this file's parsing ever stops matching the
Rust, it fails loudly rather than falling back to an unverified download.

Usage:
    scripts/provision-sandbox-runtime.py [--dest DIR] [--github-env]

Prints the `file://` URL to use as BOXLITE_RUNTIME_URL. With `--github-env` it
also appends it to $GITHUB_ENV.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import platform
import re
import sys
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
PINS = REPO_ROOT / "crates" / "rto-exec" / "src" / "runtime_pins.rs"

# One `PinnedArchive { ... }` literal. Deliberately strict: a shape this does not
# recognise is a parse failure, never a silently-skipped entry.
ENTRY = re.compile(
    r"PinnedArchive\s*\{\s*"
    r'target:\s*"(?P<target>[^"]+)"\s*,\s*'
    r'url:\s*"(?P<url>[^"]+)"\s*,\s*'
    r'sha256:\s*"(?P<sha256>[0-9a-f]{64})"\s*,\s*'
    r"bytes:\s*(?P<bytes>[0-9_]+)\s*,\s*"
    r"\}",
    re.MULTILINE,
)


def host_target() -> str:
    """The upstream target name for this machine — mirrors `runtime_target`."""
    system, machine = platform.system(), platform.machine()
    match (system, machine):
        case ("Darwin", "arm64"):
            return "darwin-arm64"
        case ("Linux", "x86_64"):
            return "linux-x64-gnu"
        case ("Linux", "aarch64"):
            return "linux-arm64-gnu"
    sys.exit(f"error: no sandbox runtime is pinned for {system}/{machine}")


def pinned_archives() -> dict[str, dict[str, object]]:
    if not PINS.is_file():
        sys.exit(f"error: cannot find the pin file at {PINS}")
    source = PINS.read_text()
    found = {
        m["target"]: {
            "url": m["url"],
            "sha256": m["sha256"],
            "bytes": int(m["bytes"].replace("_", "")),
        }
        for m in ENTRY.finditer(source)
    }
    if not found:
        sys.exit(
            f"error: parsed no PinnedArchive entries out of {PINS}.\n"
            "The pin file's shape has changed and this script no longer understands it. "
            "Fix the parsing — do not work around it by hardcoding a digest here, which "
            "is exactly the drift the single source of truth exists to prevent."
        )
    return found


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--dest",
        default=str(Path.home() / ".roteiro" / "security" / "boxlite-runtime"),
        help="where to install the archive (default: the asset cache location)",
    )
    parser.add_argument(
        "--github-env",
        action="store_true",
        help="also append BOXLITE_RUNTIME_URL to $GITHUB_ENV",
    )
    args = parser.parse_args()

    target = host_target()
    archives = pinned_archives()
    if target not in archives:
        sys.exit(
            f"error: {PINS} pins {sorted(archives)} but not {target!r} — "
            "this host cannot build --features exec-boxlite"
        )
    pin = archives[target]

    dest_dir = Path(args.dest)
    dest_dir.mkdir(parents=True, exist_ok=True)
    dest = dest_dir / "boxlite-runtime.tar.gz"

    if dest.is_file() and verified(dest, pin) is None:
        print(f"already provisioned and matching: {dest}", file=sys.stderr)
    else:
        print(f"downloading {pin['url']}", file=sys.stderr)
        with urllib.request.urlopen(pin["url"]) as response:  # noqa: S310
            body = response.read()
        # Verify BEFORE writing to the destination: a body that fails its pin
        # must never appear at the path the build script will read.
        partial = dest.with_suffix(".partial")
        partial.write_bytes(body)
        if (why := verified(partial, pin)) is not None:
            partial.unlink(missing_ok=True)
            sys.exit(f"error: the downloaded runtime does not match its pin: {why}")
        partial.replace(dest)
        print(f"verified sha256 {pin['sha256']}", file=sys.stderr)

    url = f"file://{dest}"
    if args.github_env and (github_env := os.environ.get("GITHUB_ENV")):
        with open(github_env, "a", encoding="utf-8") as handle:
            handle.write(f"BOXLITE_RUNTIME_URL={url}\n")
    print(url)


def verified(path: Path, pin: dict[str, object]) -> str | None:
    """`None` when the file matches its pin, else why it does not."""
    body = path.read_bytes()
    if len(body) != pin["bytes"]:
        return f"expected {pin['bytes']} bytes, found {len(body)}"
    digest = hashlib.sha256(body).hexdigest()
    if digest != pin["sha256"]:
        return f"expected sha256 {pin['sha256']}, found {digest}"
    return None


if __name__ == "__main__":
    main()
