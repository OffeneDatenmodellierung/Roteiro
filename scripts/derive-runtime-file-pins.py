#!/usr/bin/env python3
"""Derive the per-file sandbox-runtime pins from the pinned archives.

`crates/rto-exec/build.rs` verifies the files `boxlite` actually extracted into
its runtime directory, because those — not the archive — are what
`include_bytes!` puts in the binary. That check needs a digest per file per
platform, and **nobody should ever type one of those by hand**: fifteen hex
strings across three targets is a transcription error waiting to happen, and a
pin someone edited to make a build pass is worse than no pin at all.

So they are derived. The archives stay the single source of truth: this reads
`crates/rto-exec/src/runtime_pins.rs`, obtains each pinned archive, **verifies it
against its own pin before opening it**, hashes every member, and writes
`crates/rto-exec/src/runtime_file_pins.rs`. A `boxlite` version bump is then:
update the archive pins, run this, review the diff.

Usage:
    scripts/derive-runtime-file-pins.py               # regenerate the Rust file
    scripts/derive-runtime-file-pins.py --check       # fail if it is out of date
    scripts/derive-runtime-file-pins.py --archives DIR  # look here before downloading

Every target in `RUNTIME_ARCHIVES` is covered, including the ones this machine
cannot build for: a pin that exists only for the maintainer's laptop would make
the check vacuous on every other platform.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
import tarfile
import urllib.error
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
PINS = REPO_ROOT / "crates" / "rto-exec" / "src" / "runtime_pins.rs"
GENERATED = REPO_ROOT / "crates" / "rto-exec" / "src" / "runtime_file_pins.rs"

# Per-socket-operation timeout, for the same reason `provision-sandbox-runtime.py`
# has one: unattended, a stall that hangs is worse than a failure that explains.
DOWNLOAD_TIMEOUT_SECONDS = 120

# One `PinnedArchive { ... }` literal. Deliberately strict: a shape this does not
# recognise is a parse failure, never a silently-skipped entry. Kept in step with
# `provision-sandbox-runtime.py`, which parses the same file the same way.
ENTRY = re.compile(
    r"PinnedArchive\s*\{\s*"
    r'target:\s*"(?P<target>[^"]+)"\s*,\s*'
    r'url:\s*"(?P<url>[^"]+)"\s*,\s*'
    r'sha256:\s*"(?P<sha256>[0-9a-f]{64})"\s*,\s*'
    r"bytes:\s*(?P<bytes>[0-9_]+)\s*,\s*"
    r"\}",
    re.MULTILINE,
)

VERSION = re.compile(r'pub const RUNTIME_VERSION:\s*&str\s*=\s*"(?P<version>[^"]+)"')

# The leading path component every member carries, and which `boxlite` strips
# with `tar --strip-components=1`. Names in the runtime directory are what is
# left after it.
ARCHIVE_PREFIX = "boxlite-runtime/"


def pinned_archives(source: str) -> dict[str, dict[str, object]]:
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


def archive_bytes(target: str, pin: dict[str, object], cache: Path) -> bytes:
    """The pinned archive's bytes, from `cache` or the network, verified."""
    cached = cache / f"boxlite-runtime-{target}.tar.gz"
    if cached.is_file():
        body = cached.read_bytes()
        if verified(body, pin) is None:
            print(f"  {target}: using cached {cached}", file=sys.stderr)
            return body
        print(f"  {target}: cached copy does not match its pin, refetching", file=sys.stderr)

    print(f"  {target}: downloading {pin['url']}", file=sys.stderr)
    try:
        with urllib.request.urlopen(  # noqa: S310
            str(pin["url"]), timeout=DOWNLOAD_TIMEOUT_SECONDS
        ) as response:
            body = response.read()
    except (urllib.error.URLError, TimeoutError, OSError) as e:
        sys.exit(f"error: could not download the {target} runtime archive: {e}")

    # Verified before it is opened, never after. `tarfile` on unverified bytes is
    # exactly the exposure this whole mechanism exists to bound.
    if (why := verified(body, pin)) is not None:
        sys.exit(f"error: the {target} archive does not match its pin: {why}")

    cache.mkdir(parents=True, exist_ok=True)
    cached.write_bytes(body)
    return body


def verified(body: bytes, pin: dict[str, object]) -> str | None:
    """`None` when the bytes match their pin, else why they do not."""
    if len(body) != pin["bytes"]:
        return f"expected {pin['bytes']} bytes, found {len(body)}"
    digest = hashlib.sha256(body).hexdigest()
    if digest != pin["sha256"]:
        return f"expected sha256 {pin['sha256']}, found {digest}"
    return None


def members(target: str, body: bytes, cache: Path) -> list[tuple[str, str, int]]:
    """Every regular file the archive contributes, as (name, sha256, bytes)."""
    path = cache / f"boxlite-runtime-{target}.tar.gz"
    files: list[tuple[str, str, int]] = []
    with tarfile.open(path) as tar:
        for member in tar.getmembers():
            if member.isdir():
                continue
            if not member.isreg():
                # Not reachable for 0.9.7 — every member of all three archives is
                # a regular file or the one directory entry. Refused rather than
                # skipped: a symlink or device node appearing in a later release
                # changes what lands in the runtime directory, and this script
                # quietly dropping it would leave that file unpinned.
                sys.exit(
                    f"error: {target} archive contains {member.name!r}, which is not a "
                    f"regular file (type {member.type!r}). Extend this script deliberately "
                    "rather than letting an unpinned entry through."
                )
            if not member.name.startswith(ARCHIVE_PREFIX):
                sys.exit(
                    f"error: {target} archive member {member.name!r} does not start with "
                    f"{ARCHIVE_PREFIX!r}. boxlite extracts with `--strip-components=1`, so "
                    "the name in the runtime directory can no longer be derived."
                )
            name = member.name[len(ARCHIVE_PREFIX) :]
            if "/" in name:
                sys.exit(
                    f"error: {target} archive member {member.name!r} is nested. The runtime "
                    "directory is flat and the verifier assumes it."
                )
            handle = tar.extractfile(member)
            if handle is None:
                sys.exit(f"error: {target} archive member {member.name!r} has no contents")
            body = handle.read()
            files.append((name, hashlib.sha256(body).hexdigest(), len(body)))
    if not files:
        sys.exit(f"error: {target} archive contributes no files")
    # Sorted so the generated file is a function of the archives alone, not of
    # the order tar happens to store members in.
    return sorted(files)


def grouped(size: int) -> str:
    """`26_520_984` — the form clippy's `unreadable_literal` asks for, and the
    form `runtime_pins.rs` already writes its sizes in by hand."""
    return f"{size:_}"


def render(version: str, derived: list[tuple[str, dict[str, object], list[tuple[str, str, int]]]]) -> str:
    out = [
        "// GENERATED FILE — do not edit by hand.",
        "//",
        "// Regenerate with:",
        "//",
        "//     scripts/derive-runtime-file-pins.py",
        "//",
        "// # What these are, and why they are derived rather than written",
        "//",
        "// `boxlite` downloads the runtime archive, extracts it into its own OUT_DIR and",
        "// `include_bytes!`s **the extracted files** into the rlib. Those files are what",
        "// ends up in the binary, so those are what `build.rs` verifies — one digest per",
        "// file per platform, checked after extraction and before anything is linked.",
        "//",
        "// The archive pins in `runtime_pins.rs` remain the source of truth. This file is",
        "// a mechanical function of them: the generator verifies each archive against its",
        "// own pin before opening it, then hashes every member. A `boxlite` bump is",
        "// therefore `runtime_pins.rs` + re-run the generator + review the diff, never",
        "// fifteen hand-typed hex strings.",
        "//",
        "// Standalone on purpose — **no `use`, no `crate::` paths** — because `build.rs`",
        "// pulls it in with `include!`, exactly as it does `runtime_pins.rs`.",
        "",
        "/// One file as it must appear in `boxlite`'s extracted runtime directory.",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]",
        "pub struct PinnedFile {",
        "    /// Its name in the runtime directory — the archive member with the",
        "    /// leading `boxlite-runtime/` component stripped, as",
        "    /// `tar --strip-components=1` leaves it.",
        "    pub name: &'static str,",
        "    /// Lowercase hex SHA-256 of its contents.",
        "    pub sha256: &'static str,",
        "    /// Its exact size in bytes.",
        "    pub bytes: u64,",
        "}",
        "",
        "/// One platform's extracted runtime files.",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]",
        "pub struct PinnedRuntimeFiles {",
        "    /// The platform, as the upstream release names it.",
        "    pub target: &'static str,",
        "    /// The archive these were derived from, so a bumped archive pin that",
        "    /// nobody re-derived is a test failure rather than a silent mismatch.",
        "    pub archive_sha256: &'static str,",
        "    /// Every file the archive contributes, sorted by name.",
        "    pub files: &'static [PinnedFile],",
        "}",
        "",
        "/// The `boxlite` release these were derived from.",
        f'pub const RUNTIME_FILES_VERSION: &str = "{version}";',
        "",
        "/// Every pinned platform's extracted runtime files.",
        "pub const RUNTIME_FILES: &[PinnedRuntimeFiles] = &[",
    ]
    for target, pin, files in derived:
        out.append("    PinnedRuntimeFiles {")
        out.append(f'        target: "{target}",')
        out.append(f'        archive_sha256: "{pin["sha256"]}",')
        out.append("        files: &[")
        for name, sha256, size in files:
            out.append("            PinnedFile {")
            out.append(f'                name: "{name}",')
            out.append(f'                sha256: "{sha256}",')
            out.append(f"                bytes: {grouped(size)},")
            out.append("            },")
        out.append("        ],")
        out.append("    },")
    out += [
        "];",
        "",
        "/// The extracted-file pins for an upstream target name.",
        "#[must_use]",
        "pub fn runtime_files_for(target: &str) -> Option<&'static PinnedRuntimeFiles> {",
        "    let mut index = 0;",
        "    // A plain loop rather than an iterator, matching `runtime_pins.rs`: this file",
        "    // is `include!`d into a build script, where keeping to the language core is",
        "    // the point.",
        "    while index < RUNTIME_FILES.len() {",
        "        if RUNTIME_FILES[index].target.as_bytes() == target.as_bytes() {",
        "            return Some(&RUNTIME_FILES[index]);",
        "        }",
        "        index += 1;",
        "    }",
        "    None",
        "}",
        "",
    ]
    return "\n".join(out)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail if the generated file is not what this would write",
    )
    parser.add_argument(
        "--archives",
        default=str(Path.home() / ".cache" / "roteiro" / "runtime-archives"),
        help="where to cache the downloaded archives",
    )
    args = parser.parse_args()

    if not PINS.is_file():
        sys.exit(f"error: cannot find the pin file at {PINS}")
    source = PINS.read_text()

    match = VERSION.search(source)
    if match is None:
        sys.exit(f"error: could not read RUNTIME_VERSION out of {PINS}")
    version = match["version"]

    archives = pinned_archives(source)
    cache = Path(args.archives)
    print(f"deriving from {len(archives)} pinned archive(s):", file=sys.stderr)

    derived = []
    for target in sorted(archives):
        pin = archives[target]
        body = archive_bytes(target, pin, cache)
        files = members(target, body, cache)
        print(f"  {target}: {len(files)} file(s)", file=sys.stderr)
        derived.append((target, pin, files))

    rendered = render(version, derived)

    if args.check:
        current = GENERATED.read_text() if GENERATED.is_file() else ""
        if current != rendered:
            sys.exit(
                f"error: {GENERATED.relative_to(REPO_ROOT)} is not what the archives say it "
                "should be.\nRe-run scripts/derive-runtime-file-pins.py and review the diff."
            )
        print("generated file is up to date", file=sys.stderr)
        return

    GENERATED.write_text(rendered)
    print(f"wrote {GENERATED.relative_to(REPO_ROOT)}", file=sys.stderr)


if __name__ == "__main__":
    main()
