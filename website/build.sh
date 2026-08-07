#!/bin/sh
# Assemble the website into website/dist. POSIX sh; no dependencies.
set -eu
cd "$(dirname "$0")"
rm -rf dist
mkdir -p dist/adr
cp -r public/. dist/
# Publish ADR markdown verbatim for now; roteiro render will HTML-ify later.
cp ../docs/adr/*.md dist/adr/
# Simple ADR index page linking the markdown files.
{
  echo '<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Roteiro ADRs</title></head><body><h1>Architecture Decision Records</h1><ul>'
  for f in dist/adr/*.md; do
    b=$(basename "$f")
    [ "$b" = "README.md" ] && continue
    echo "<li><a href=\"$b\">$b</a></li>"
  done
  echo '</ul><p><a href="../">Home</a></p></body></html>'
} > dist/adr/index.html
echo "built website/dist"
