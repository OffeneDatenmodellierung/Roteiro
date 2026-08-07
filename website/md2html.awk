# md2html.awk — minimal Markdown → HTML for Roteiro ADRs.
# Covers the constructs the house ADR/blueprint style uses: YAML frontmatter,
# ATX headings, GFM pipe tables, unordered lists, fenced code blocks,
# paragraphs, and inline code / bold / italic / links. Deliberately small and
# dependency-free (POSIX awk, no gawk/gensub) until `roteiro render` supersedes it.

function esc(s) {
  gsub(/&/, "\\&amp;", s)
  gsub(/</, "\\&lt;", s)
  gsub(/>/, "\\&gt;", s)
  return s
}

function trim(s) {
  gsub(/^[[:space:]]+/, "", s)
  gsub(/[[:space:]]+$/, "", s)
  return s
}

# Inline formatting. Escapes HTML first, then protects code spans so their
# contents are never treated as markup, then applies links/bold/italic.
function inl(s,   m, inner, i, p, ph, n, code, cb, text, url) {
  s = esc(s)
  n = 0
  while (match(s, /`[^`]+`/)) {
    n++
    code[n] = substr(s, RSTART + 1, RLENGTH - 2)
    s = substr(s, 1, RSTART - 1) "\001" n "\002" substr(s, RSTART + RLENGTH)
  }
  while (match(s, /\[[^]]+\]\([^)]+\)/)) {
    m = substr(s, RSTART, RLENGTH)
    cb = index(m, "]")
    text = substr(m, 2, cb - 2)
    url = substr(m, cb + 2, length(m) - cb - 2)
    gsub(/"/, "\\&quot;", url)
    gsub(/'/, "\\&#39;", url)
    s = substr(s, 1, RSTART - 1) "<a href=\"" url "\">" text "</a>" substr(s, RSTART + RLENGTH)
  }
  while (match(s, /\*\*[^*]+\*\*/)) {
    m = substr(s, RSTART, RLENGTH)
    inner = substr(m, 3, length(m) - 4)
    s = substr(s, 1, RSTART - 1) "<strong>" inner "</strong>" substr(s, RSTART + RLENGTH)
  }
  while (match(s, /\*[^*]+\*/)) {
    m = substr(s, RSTART, RLENGTH)
    inner = substr(m, 2, length(m) - 2)
    s = substr(s, 1, RSTART - 1) "<em>" inner "</em>" substr(s, RSTART + RLENGTH)
  }
  while (match(s, /_[^_]+_/)) {
    m = substr(s, RSTART, RLENGTH)
    inner = substr(m, 2, length(m) - 2)
    s = substr(s, 1, RSTART - 1) "<em>" inner "</em>" substr(s, RSTART + RLENGTH)
  }
  for (i = 1; i <= n; i++) {
    ph = "\001" i "\002"
    p = index(s, ph)
    if (p > 0)
      s = substr(s, 1, p - 1) "<code>" code[i] "</code>" substr(s, p + length(ph))
  }
  return s
}

function emit_cells(row, tag,   n, c, i, out) {
  sub(/^[[:space:]]*\|/, "", row)
  sub(/\|[[:space:]]*$/, "", row)
  n = split(row, c, /\|/)
  out = ""
  for (i = 1; i <= n; i++)
    out = out "<" tag ">" inl(trim(c[i])) "</" tag ">"
  return out
}

function cells_empty(row,   n, c, i) {
  sub(/^[[:space:]]*\|/, "", row)
  sub(/\|[[:space:]]*$/, "", row)
  n = split(row, c, /\|/)
  for (i = 1; i <= n; i++)
    if (trim(c[i]) != "") return 0
  return 1
}

{ line = $0; sub(/\r$/, "", line); L[NR] = line }

END {
  i = 1
  # Skip YAML frontmatter delimited by leading/closing --- lines.
  if (L[1] == "---") { i = 2; while (i <= NR && L[i] != "---") i++; i++ }

  while (i <= NR) {
    line = L[i]

    if (line ~ /^```/) {
      print "<pre><code>"
      i++
      while (i <= NR && L[i] !~ /^```/) { print esc(L[i]); i++ }
      print "</code></pre>"
      i++
      continue
    }

    if (line ~ /^[[:space:]]*$/) { i++; continue }

    if (line ~ /^#{1,6}[[:space:]]/) {
      n = 0
      while (substr(line, n + 1, 1) == "#") n++
      print "<h" n ">" inl(trim(substr(line, n + 1))) "</h" n ">"
      i++
      continue
    }

    # GFM pipe table: a pipe row immediately followed by a --- separator row.
    if (line ~ /^[[:space:]]*\|.*\|[[:space:]]*$/ && (i + 1) <= NR &&
        L[i+1] ~ /^[[:space:]]*\|[-:| ]+\|[[:space:]]*$/ && L[i+1] ~ /-/) {
      print "<table>"
      if (!cells_empty(line))
        print "<thead><tr>" emit_cells(line, "th") "</tr></thead>"
      i += 2
      print "<tbody>"
      while (i <= NR && L[i] ~ /^[[:space:]]*\|.*\|[[:space:]]*$/) {
        print "<tr>" emit_cells(L[i], "td") "</tr>"
        i++
      }
      print "</tbody></table>"
      continue
    }

    if (line ~ /^[[:space:]]*[-*][[:space:]]/) {
      print "<ul>"
      while (i <= NR && L[i] ~ /^[[:space:]]*[-*][[:space:]]/) {
        item = L[i]
        sub(/^[[:space:]]*[-*][[:space:]]+/, "", item)
        print "<li>" inl(item) "</li>"
        i++
      }
      print "</ul>"
      continue
    }

    # Paragraph: join wrapped lines until a blank line or a new block starts.
    para = inl(line)
    i++
    while (i <= NR && L[i] !~ /^[[:space:]]*$/ && L[i] !~ /^#{1,6}[[:space:]]/ &&
           L[i] !~ /^[[:space:]]*[-*][[:space:]]/ && L[i] !~ /^```/ &&
           L[i] !~ /^[[:space:]]*\|.*\|[[:space:]]*$/) {
      para = para " " inl(L[i])
      i++
    }
    print "<p>" para "</p>"
  }
}
