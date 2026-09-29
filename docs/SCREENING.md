# What the content screen checks, and what it does not

Text that Roteiro did not write — a peer's OKF bundle, a PDF dropped into a
corpus — is screened before it becomes a node's `meta.content`, because that
field is returned verbatim to a language model by the `search`, `explain` and
`context` tools. A body carrying instructions aimed at that model reaches it by
exactly that route.

This page exists because **a screen that runs, passes, and implies a check it
did not perform is worse than no screen**: it reads as coverage. What follows is
the list of what a `pass` does and does not mean, per format.

## The three verdicts

| verdict | what happens to the text |
|---|---|
| `pass` | stored unchanged |
| `quarantine` | the node is kept; the suspect part is removed, or the whole body is withheld |
| `block` | nothing is stored, and the document contributes no content |

`block` requires **concealment and direction together**. Each half alone is
deliberately not enough: instruction-shaped prose that is *visible* is usually a
document *about* prompt injection, and hidden text on its own is usually an
editor's leftover. Text arranged so that a human reviewing the document cannot
see it while a model reading the same file can, carrying an instruction, is
neither — and that is the case worth refusing outright.

## Markdown and other prose

Checked:

- **invisible codepoints** — zero-width characters, joiners, bidirectional
  controls (the "Trojan Source" class), the invisible-ASCII tag block, and C0/C1
  controls;
- **presentation-hidden regions** — HTML comments, and inline HTML whose own
  attributes hide it (`display:none`, `visibility:hidden`, `opacity:0`,
  `font-size:0`, `color:transparent`, `aria-hidden`, the boolean `hidden`);
- **model directives** — phrases that read as an instruction addressed to a
  language model, and chat-template markers.

Not checked: homoglyphs and confusables; encoded payloads (base64, hex,
percent-encoding are not decoded and re-screened); directives in any language
other than English; a CSS cascade (hiding is read from a tag's own attributes,
not from a `<style>` block elsewhere); and nothing already stored before the
screen existed is re-screened.

## PDF

A PDF is screened twice over: once as the flat text the extractor produces, by
every rule above, and once against **the page's own content stream**, which is
where a PDF's concealment mechanisms actually live. The extractor discards
colour, position, size and rendering mode before returning, so without the
second read a concealed payload arrives looking exactly like the prose around
it.

Checked, from the content stream:

| class | signal |
|---|---|
| invisible text | text rendering mode 3 or 7 — neither filled nor stroked |
| white-on-white | fill colour equal to whatever is painted behind the glyph |
| off-page | glyph origin outside the page `MediaBox` |
| off-page | glyph origin outside the page `CropBox` |
| degenerate size | effective size below 1 pt once the text and current transformation matrices are composed |

The read is deterministic and in-process: a pure function of the bytes, with no
clock, no network and no model. That is what makes the verdict eligible to be a
graph fact, and therefore what lets a PDF reach `block` at all.

### Not checked, and why

**Text painted over by an opaque image.** An ordinary black, in-bounds,
normally-sized text layer with a picture drawn on top of it. The content-stream
read *can see the overlap* and deliberately does not report it, because whether
an image hides what is under it is a question about opacity — soft masks,
transparency groups, `/Decode` — and a mostly-transparent image covers nothing.
Reporting every overlap would fire on watermarks, logos and any figure whose box
crosses a baseline.

The obvious alternative is to rasterise the page and compare an OCR read against
the text layer. That was measured on this repository and **failed**: on six
clean pages from real papers the metric's noise floor reached a run of 4
unmatched words against a signal of 3 from the concealment fixture, so no
threshold separates them. The cause is systematic — the failing page's arXiv
stamp is printed rotated down the left margin, the OCR engine does not read
rotated text, and 237 of 310 readable first pages in that corpus carry the same
stamp.

**Text hidden by clipping or by a zero alpha.** Graphics-state save/restore and
the current transformation matrix are tracked; clipping paths (`W`, `W*`),
`ExtGState` alpha (`gs`), soft masks and blend modes are not.

**A lying text layer that is not directive-shaped.** An invisible text layer
over a page image is how every searchable scan is built — OCR output drawn in
mode 3 so the page is selectable. Reporting its *existence* would quarantine the
body of every scanned paper in a corpus, so only its **content** is screened. An
invisible layer that disagrees with the page, in words that are not instructions
to a model, passes.

**A text run whose font cannot be decoded.** Directives are matched against text
decoded through the font's own encoding. A subset font with no usable encoding
decodes to nothing legible, so the concealment *class* is still reported and the
document still quarantines — but the `block` half cannot fire on text nobody can
read.

**No glyph advance.** The origin of each text-showing operator is tested, not
the box the run sweeps out. A line that begins on the page and runs off the edge
is not reported.

## What a PDF `pass` therefore means

That the document's text carries no model directive, no invisible codepoints, and
no text concealed by render mode, fill colour, page position or font size.

It does **not** mean the page shows everything its text layer says. Treat a PDF
`pass` as a narrower statement than a markdown `pass`, and treat a PDF from an
untrusted source as reviewed by a human who has seen the *rendering*, not the
text layer.
