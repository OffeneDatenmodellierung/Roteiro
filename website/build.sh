#!/bin/sh
# Build the Roteiro docs site into website/dist.
#
# The site is a build-output of the graph: `roteiro render docs` renders each
# ADR with a real CommonMark parser and copies the static theme/assets from
# website/public. This replaces the former shell + md2html.awk stopgap.
#
# Requires the Rust toolchain (the Cloudflare Pages build image and CI both
# provide it). Run from anywhere in the repo.
set -eu
cd "$(dirname "$0")/.."
cargo run --quiet --package roteiro -- render docs --out website/dist
