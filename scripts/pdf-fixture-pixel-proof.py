#!/usr/bin/env python3
"""Prove that each committed PDF concealment fixture really does conceal.

`crates/rto-graph/tests/fixtures/pdf/` holds pairs: a fixture that plants one
concealment mechanism carrying a directive payload, and a **matched control** —
byte-for-byte the same page with the payload drawing removed. This script
renders both and counts the pixels that differ.

**Why a separate script and not a test.** The comparison needs a PDF
rasteriser, and this repository deliberately has none: `is_image()` admits
`png | jpg | jpeg` only, and nothing in the tree links pdfium, mupdf, poppler
or ghostscript. So the oracle here is macOS's own CoreGraphics, via `sips`,
which makes this a **measurement a maintainer runs**, not a gate CI can. The
fixtures themselves are gated for byte-reproducibility by
`cargo test -p rto-graph --test pdf_fixtures`, which runs everywhere.

**What a passing run means.** Two numbers per fixture, and both matter:

* `differing` — pixels where the fixture and its control disagree once both
  are composited onto white paper. Zero means the planted payload put nothing
  on the page, which is the definition of concealed.
* `ink` — dark pixels in the fixture. Must be **non-zero**, because a fixture
  whose *visible* half is also invisible diffs to zero against its control and
  proves the opposite of what it claims. That check is not hypothetical: it
  caught exactly that bug while these fixtures were being built, when a white
  page-background fill leaked its colour into the visible line.

Compositing onto white is likewise load-bearing. A PDF page is transparent
where nothing paints it, so a naive RGB comparison reports white-on-white text
as 4,371 differing pixels against a transparent control — the payload reads as
*visible* by the metric while being invisible to a reader.

Usage:

    scripts/pdf-fixture-pixel-proof.py
    scripts/pdf-fixture-pixel-proof.py --keep /tmp/renders
"""

from __future__ import annotations

import argparse
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

FIXTURES = Path(__file__).resolve().parent.parent / "crates/rto-graph/tests/fixtures/pdf"

# The two degenerate-size fixtures are the only ones that do not diff to exactly
# zero, and that is a property of the class rather than a defect in them.
# Concealment by size means the text cannot be **read**, not that it leaves no
# mark: a 62-character run at 0.01 pt (fixture 04) and at 0.04 pt (fixture 13)
# collapses to 1 and 2 anti-aliased pixels respectively on a 1,224-pixel-tall
# render. Nothing is legible in either, and nothing survives print resolution.
#
# The allowance is per-fixture and explicit rather than a global tolerance, so a
# fixture that starts leaking is a failure rather than a number sitting under a
# threshold nobody re-reads.
ALLOWED_DIFFERING = {"04-degenerate-size": 1, "13-ctm-shrunk-text": 2}


def decode_png(path: Path) -> tuple[int, int, int, bytes]:
    """Decode a non-interlaced 8-bit PNG with the standard library alone."""
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path} is not a PNG")
    offset, idat, header, palette = 8, b"", None, None
    while offset < len(data):
        (length,) = struct.unpack(">I", data[offset : offset + 4])
        kind = data[offset + 4 : offset + 8]
        body = data[offset + 8 : offset + 8 + length]
        offset += 12 + length
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
        elif kind == b"PLTE":
            palette = body
        elif kind == b"IEND":
            break
    width, height, depth, colour, _comp, _filt, interlace = header
    if depth != 8 or interlace != 0:
        raise ValueError(f"{path}: only 8-bit non-interlaced PNG is handled")
    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[colour]
    raw = zlib.decompress(idat)
    stride = width * channels
    out = bytearray(height * stride)
    previous = bytearray(stride)
    pos = 0
    for row in range(height):
        filter_type = raw[pos]
        pos += 1
        line = bytearray(raw[pos : pos + stride])
        pos += stride
        for i in range(stride):
            left = line[i - channels] if i >= channels else 0
            up = previous[i]
            up_left = previous[i - channels] if i >= channels else 0
            if filter_type == 1:
                line[i] = (line[i] + left) & 0xFF
            elif filter_type == 2:
                line[i] = (line[i] + up) & 0xFF
            elif filter_type == 3:
                line[i] = (line[i] + ((left + up) >> 1)) & 0xFF
            elif filter_type == 4:
                predictor = left + up - up_left
                da, db, dc = (
                    abs(predictor - left),
                    abs(predictor - up),
                    abs(predictor - up_left),
                )
                nearest = left if (da <= db and da <= dc) else (up if db <= dc else up_left)
                line[i] = (line[i] + nearest) & 0xFF
        out[row * stride : (row + 1) * stride] = line
        previous = line
    if colour == 3:
        expanded = bytearray()
        for index in out:
            expanded += palette[index * 3 : index * 3 + 3]
        return width, height, 3, bytes(expanded)
    return width, height, channels, bytes(out)


def on_white(buf: bytes, channels: int, index: int) -> tuple[int, int, int]:
    """The RGB a reader sees, compositing pixel `index` onto white paper."""
    px = buf[index * channels : (index + 1) * channels]
    if channels == 1:
        return (px[0], px[0], px[0])
    if channels == 2:
        alpha = px[1]
        value = (px[0] * alpha + 255 * (255 - alpha)) // 255
        return (value, value, value)
    if channels == 3:
        return (px[0], px[1], px[2])
    alpha = px[3]
    return tuple((px[k] * alpha + 255 * (255 - alpha)) // 255 for k in range(3))


def render(pdf: Path, png: Path) -> None:
    subprocess.run(
        ["sips", "-s", "format", "png", "-Z", "1224", str(pdf), "--out", str(png)],
        check=True,
        capture_output=True,
    )


def compare(fixture_png: Path, control_png: Path) -> tuple[int, int, str]:
    fw, fh, fc, fixture = decode_png(fixture_png)
    cw, ch, cc, control = decode_png(control_png)
    if (fw, fh) != (cw, ch):
        return -1, -1, f"dimension mismatch {fw}x{fh} vs {cw}x{ch}"
    differing = 0
    ink = 0
    for i in range(fw * fh):
        pixel = on_white(fixture, fc, i)
        if pixel != on_white(control, cc, i):
            differing += 1
        if pixel[0] < 128:
            ink += 1
    return differing, ink, f"{fw}x{fh}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--keep", type=Path, help="write the renders here instead of a temp dir")
    args = parser.parse_args()

    if sys.platform != "darwin":
        print("needs macOS `sips`: this repository ships no PDF rasteriser", file=sys.stderr)
        return 2

    pairs = sorted(
        (p, p.with_name(p.stem + "-control.pdf"))
        for p in FIXTURES.glob("*.pdf")
        if not p.stem.endswith("-control")
    )
    if not pairs:
        print(f"no fixtures in {FIXTURES}", file=sys.stderr)
        return 2

    with tempfile.TemporaryDirectory() as tmp:
        out = args.keep or Path(tmp)
        out.mkdir(parents=True, exist_ok=True)
        failures = 0
        print(f"{'fixture':<24} {'size':>10} {'differing':>10} {'ink':>7}  verdict")
        for fixture, control in pairs:
            if not control.exists():
                # A fixture with no matched control claims no concealment: it is
                # a negative case, present so a detector that flags everything
                # fails. Nothing to pixel-diff, and its absence is not a defect.
                print(f"{fixture.stem:<24} {'':>10} {'':>10} {'':>7}  negative (claims no concealment)")
                continue
            fixture_png = out / f"{fixture.stem}.png"
            control_png = out / f"{control.stem}.png"
            render(fixture, fixture_png)
            render(control, control_png)
            differing, ink, size = compare(fixture_png, control_png)
            allowed = ALLOWED_DIFFERING.get(fixture.stem, 0)
            ok = 0 <= differing <= allowed and ink > 0
            note = "concealed" if ok else ("VISIBLE" if differing > allowed else "NO INK")
            if differing < 0:
                note = size
            print(f"{fixture.stem:<24} {size:>10} {differing:>10} {ink:>7}  {note}")
            failures += 0 if ok else 1
        if failures:
            print(f"\n{failures} fixture(s) do not conceal what they claim to", file=sys.stderr)
        return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
