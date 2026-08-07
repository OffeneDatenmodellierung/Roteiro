#!/bin/sh
# Assemble the website into website/dist. POSIX sh; no dependencies.
set -eu
cd "$(dirname "$0")"
rm -rf dist
mkdir -p dist/adr
cp -r public/. dist/

# Shared page chrome. $1 = relative path to site root (e.g. "../").
head_html() { # $1 root, $2 title
  printf '%s' '<!doctype html><html lang="en"><head><meta charset="utf-8">'
  printf '%s' '<meta name="viewport" content="width=device-width, initial-scale=1">'
  printf '<link rel="icon" href="%sfavicon.svg" type="image/svg+xml">' "$1"
  printf '<link rel="stylesheet" href="%sstyle.css">' "$1"
  printf '<title>%s</title></head><body>' "$(printf '%s' "$2" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g')"
}
foot_html() { # $1 root
  printf '<p class="backlink"><a href="%s">← Back to roteiro.dev</a></p>' "$1"
  printf '%s' '<footer>Dual-licensed MIT OR Apache-2.0 · The Roteiro Project Team</footer></body></html>'
}

# Render each ADR markdown to a themed HTML page with nav + back links, and
# collect an <li> for the index. `roteiro render` will HTML-ify this later.
index_items=""
for f in ../docs/adr/*.md; do
  b=$(basename "$f")
  [ "$b" = "README.md" ] && continue
  slug=${b%.md}
  # First H1 after any YAML frontmatter is the page title; fall back to slug.
  title=$(awk 'NR==1&&$0=="---"{fm=1;next} fm&&$0=="---"{fm=0;next} fm{next} /^# /{sub(/^# /,"");print;exit}' "$f")
  [ -z "$title" ] && title=$slug
  {
    head_html "../" "$title — Roteiro"
    printf '<p class="nav"><a href="../">← Roteiro home</a> · <a href="./">All ADRs</a></p>'
    awk -f md2html.awk "$f"
    foot_html "../"
  } > "dist/adr/$slug.html"
  index_items="$index_items<li><a href=\"$slug.html\">$title</a></li>"
done

# Themed ADR index page.
{
  head_html "../" "Architecture Decision Records — Roteiro"
  printf '%s' '<p class="nav"><a href="../">← Roteiro home</a></p>'
  printf '%s' '<h1>Architecture Decision Records</h1><ul>'
  printf '%s' "$index_items"
  printf '%s' '</ul>'
  foot_html "../"
} > dist/adr/index.html

echo "built website/dist"
