#!/bin/sh
# Build the Roteiro docs site into website/dist.
#
# The site is a build-output of the graph: `roteiro render docs` renders each
# ADR with a real CommonMark parser and copies the static theme/assets from
# website/public. This replaces the former shell + md2html.awk stopgap.
#
# The workspace MSRV is 1.94. Build environments whose default toolchain is
# older (e.g. Cloudflare Pages) — or which ship no Rust at all — are handled by
# bootstrapping the pinned toolchain here, so the Git-integration deploy needs
# no special dashboard configuration.
set -eu
cd "$(dirname "$0")/.."

MSRV=1.94
if command -v rustup >/dev/null 2>&1; then
  rustup toolchain install "$MSRV" --profile minimal 2>/dev/null || true
  CARGO="cargo +$MSRV"
elif command -v cargo >/dev/null 2>&1; then
  CARGO="cargo"
else
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain "$MSRV"
  . "$HOME/.cargo/env"
  CARGO="cargo +$MSRV"
fi

$CARGO run --quiet --package roteiro -- render docs --out website/dist
