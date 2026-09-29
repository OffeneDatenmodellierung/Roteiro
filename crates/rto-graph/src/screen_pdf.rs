//! PDF-native concealment, read from the content stream (#813).
//!
//! # The hole this closes
//!
//! [`crate::screen`] decides `block` on **concealment and direction together**,
//! and every concealment mechanism it knows is an HTML construct: `display:none`,
//! the boolean `hidden` attribute, an HTML comment, zero-width codepoints, bidi
//! controls. A PDF conceals text by a different vocabulary entirely, and
//! `pdf_extract::extract_text_from_mem` hands the screener a **flat string with
//! every rendering decision already discarded** — so the payload arrives looking
//! exactly like the prose around it.
//!
//! The consequence, verified on #813 before this module existed: a PDF could
//! reach `block` only by carrying zero-width characters. Concealment by colour,
//! position, size or render mode was **structurally invisible** to the screen, so
//! a PDF topped out at `quarantine` — the rule silently weakened by document
//! format, which is the complaint #813 was filed over.
//!
//! This module reads the mechanism directly, out of the page's own content
//! stream, and hands what it finds back to [`crate::screen`] as
//! [`ConcealedText`]. `block` is reachable for a PDF from here on.
//!
//! # Deterministic, in-process, and no new dependency
//!
//! A rendered-pixel oracle could only ever report *"the text layer and the page
//! disagree"*, and not reproducibly. A content-stream read reports the
//! **mechanism**, as a pure function of the bytes: no clock, no network, no
//! model, no filesystem. That is what makes the verdict eligible to be a
//! `derived` graph fact at all (ADR-0019 §5), and therefore what lets it reach
//! `block` rather than being advisory.
//!
//! `pdf-extract` already re-exports `lopdf` (`pub use lopdf::*`), so the content
//! stream, the page dictionary and the font encodings are all reachable through
//! the dependency the `pdf-text` feature already pulls. Nothing is added to the
//! tree.
//!
//! # Why not `OutputDev`
//!
//! `pdf-extract`'s extraction callback looks like the natural seam and is not.
//! It never parses `Tr` at all, and `gs.fill_color` reaches only `fill()` for
//! paths — never `output_character` — so **render mode and fill colour are not
//! reachable through it**. `begin_page` is handed `MediaBox` and the art box but
//! not `CropBox`, which is why text inside `MediaBox` and outside `CropBox` is
//! invisible to it. It would buy degenerate size and out-of-`MediaBox`, and
//! nothing else. The raw stream buys all six.
//!
//! # What is detected
//!
//! | class | signal |
//! |---|---|
//! | invisible text | text rendering mode 3 or 7 — fills nothing, strokes nothing |
//! | white-on-white | fill colour equal to whatever is painted behind the glyph |
//! | off-page | glyph origin outside `MediaBox` |
//! | off-page | glyph origin outside `CropBox` |
//! | degenerate size | effective size below [`MIN_LEGIBLE_SIZE`] once the text and current transformation matrices are composed |
//! | lying layer | an invisible text run over a page image — **content screened, existence not reported**; see below |
//!
//! # The state model, and why it is the whole of it
//!
//! Whether a glyph marks the page is a question about the **graphics state**,
//! and a reader that tracks a subset of it answers a different question than it
//! thinks. Three of the classes above were bypassable for exactly that reason
//! until the #912 review, so what is tracked is enumerated here against the
//! specification's own tables rather than against what came to mind.
//!
//! **Graphics state (PDF 32000-1 Table 52).** Tracked: the CTM; the fill and
//! stroke colours *and the spaces they are read in*; and the text state below —
//! all of them saved by `q` and restored by `Q`, because that is what §9.3 says
//! and a reader that kept any of them outside the stack reads a `Q` as leaving
//! them alone. Not tracked, and each is a decision:
//!
//! - **the clipping path** (`W`, `W*`) — can conceal, declared below;
//! - **`ExtGState`** (`gs`: constant alpha, soft mask, blend mode) — can
//!   conceal, declared below;
//! - **stroke geometry** (line width, cap, join, miter, dash) — cannot hide a
//!   *filled* glyph at all, and hides a stroked one only through a degenerate
//!   dash pattern, which is the clipping class one operator over and declared
//!   with it;
//! - **rendering intent, flatness, smoothness, stroke adjustment, overprint** —
//!   these change how a mark is *rendered*, never whether there is one. Nothing
//!   in this set can carry a concealment, which is why they are absent rather
//!   than declared.
//!
//! **Text state (Table 103).** Tracked: `Tf` and its size, `Tz`, `Ts`, `TL` and
//! `Tr`. These are **graphics** state, so `q`/`Q` carry them and `BT` does not
//! reset them — getting that wrong is wrong in both directions, and fixtures 15
//! and 16 are the two. Three of them enter the *rendering matrix* of §9.4.4
//! rather than being separate tests; see [`TextParameters::rendering_matrix`],
//! which is why `Tz` and `Ts` are closed by getting the transform right instead
//! of by two more special cases. Not tracked: `Tc` and `Tw`, which change the
//! **advance** and not the origin (see the glyph-advance note below), and `Tk`,
//! which is a transparency-group property and belongs to the alpha class.
//!
//! **Text object state (§9.4.1).** The text and text-line matrices, reset by
//! `BT` and *not* saved by `q`/`Q` — the one place where the opposite rule
//! applies, and the reason [`TextState`] is a separate type from
//! [`TextParameters`].
//!
//! # What is deliberately **not** detected, and why
//!
//! Stated here, in [`crate::screen`]'s own non-goals list, in `docs/`, and in
//! what `roteiro sync` prints when it accepts a PDF — on both its human and its
//! `--json` surface. A screen that runs, passes, and implies a check it did not
//! perform is worse than no screen: it reads as coverage. #813 and ADR-0025
//! both settle on the same standard — *a narrower guard that states its limits
//! beats a broad one that quietly checks nothing.*
//!
//! **Text painted over by an opaque image.** An ordinary black, 12 pt, in-bounds
//! text layer with a picture drawn on top of it afterwards. Nothing in that
//! content stream is anomalous, and this reader **can see the overlap and still
//! does not report it** — which is a stronger statement than "we cannot see it",
//! and a deliberate choice rather than a gap:
//!
//! - Whether an image *hides* what is under it is a question about opacity, and
//!   opacity is not in the geometry. It needs `/SMask`, `/Mask`, `/Decode` and
//!   the transparency group resolved — a mostly-transparent PNG covers nothing.
//! - Treating every image-over-text overlap as concealment fires on watermarks,
//!   logos, page furniture and any figure whose box crosses a baseline origin.
//!
//! The cheap alternative was **measured, on this repository, and it failed**:
//! rasterise and compare an OCR read against the text layer. On six clean pages
//! from real papers the metric's noise floor reached a run of 4 unmatched words,
//! against a signal of 3 from the concealment fixture — *no threshold exists*.
//! The cause was systematic rather than unlucky: the failing page's arXiv stamp
//! is printed rotated down the left margin, `ocrs` does not read rotated text,
//! and **237 of 310 first pages in the corpus carry that stamp**. Nothing in
//! this repository rasterises a PDF either, so the cheap version would also need
//! a rasteriser it does not have.
//!
//! **An invisible text layer over a page image is not reported as concealment.**
//! That is how every searchable scan in the world is built: OCR output drawn in
//! mode 3 over the scanned image so the page is selectable and findable.
//! Reporting its *presence* would quarantine the body of every scanned paper in
//! a corpus. What separates an honest sidecar from a lying one is whether the
//! invisible layer agrees with the page — and that is the pixel oracle above,
//! measured and rejected. So the content of such a layer **is** screened, and
//! reaches `block` if it carries a directive; its mere existence is not a
//! finding. A lying layer whose lie is not directive-shaped therefore passes.
//!
//! **No opacity, no clipping, no transparency groups.** Text made invisible by
//! a constant alpha of zero, a soft mask, or a clipping path that excludes it is
//! not detected. This is the largest declared gap and it is a *real* bypass, not
//! an unreachable one — it is declared rather than closed because each needs a
//! resource resolved (`ExtGState`, and a path-intersection model for clipping)
//! and neither is a small addition to a content-stream walk.
//!
//! **No glyph advance.** The origin of each text-showing operator is tested, not
//! the box the run sweeps out. A line that starts inside the page and runs off
//! the edge is not reported, and neither is one pushed off by `Tc`/`Tw`.
//!
//! **No form XObjects.** `Do` on a form is not followed into its own content
//! stream, so concealment inside one is not seen. Its rectangle is likewise not
//! recorded as page coverage, which is the safe direction: claiming an opaque
//! region this reader never looked inside would *suppress* findings.
//!
//! **Concealed text is only recoverable when its font is.** Directives are
//! matched against text decoded through the font's own encoding. A subset font
//! with no usable `/Encoding` or `/ToUnicode` decodes to nothing legible, so the
//! *class* is still reported — and the document still quarantines — but the
//! `block` half cannot fire. That failure direction is the safe one, and it is
//! the reason `block` is not claimed to be universally reachable, only reachable.

use std::collections::BTreeMap;

use pdf_extract::{Dictionary, Document, Object, ObjectId};

use crate::screen::ConcealedText;

/// The smallest effective text size, in PDF points, this reader treats as
/// legible.
///
/// One point is 1/72 inch: below it, a glyph cannot survive being rendered at
/// any resolution a reader or an OCR pass uses. The fixture plants 0.01 pt —
/// 1/7200 inch — which puts a single anti-aliased pixel on a 1,224-pixel-tall
/// render of a US Letter page, and nothing at print resolution.
///
/// The comparison is against the **effective** size, not the `Tf` operand. A
/// generator emitting `/F1 1 Tf` with a `12 0 0 12 x y Tm` text matrix is
/// entirely ordinary, and a reader that looked at the operand alone would report
/// every such document as concealing its whole body.
pub(crate) const MIN_LEGIBLE_SIZE: f64 = 1.0;

/// How close a text colour must be to what is behind it to count as hidden in
/// it, per channel, on a 0..=1 scale.
///
/// Not zero: a generator that writes `0.996 0.996 0.996 rg` over a white page
/// has concealed its text just as thoroughly as one that writes `1 1 1 rg`, and
/// an exact-equality test is an allowlist of one spelling — the shape
/// `allowlist-the-property-not-the-syntax` exists to avoid. 1/255 is the
/// finest distinction an 8-bit render can make at all.
const COLOUR_TOLERANCE: f64 = 1.0 / 255.0;

/// Cap on the concealed text collected from one document.
///
/// Bounded work, and generous: `cap_content` admits 1,500 characters to a node,
/// so this is two orders of magnitude more than the visible half ever keeps. It
/// exists so a pathological document cannot make the directive scan quadratic in
/// its own size, not to limit what can be found.
const MAX_CONCEALED_CHARS: usize = 256 * 1024;

/// One thing the content stream does that hides text, as the stable token
/// recorded in `meta.screen`.
///
/// Free functions rather than an enum: each is used exactly once, at the site
/// that detects it, and the set is closed by what a content stream can express
/// rather than by a type this crate owns.
pub(crate) mod mechanism {
    /// Text rendering mode 3 or 7 — neither filled nor stroked.
    pub(crate) const INVISIBLE_RENDER_MODE: &str = "pdf-invisible-render-mode";
    /// Filled in the colour of whatever is painted behind it.
    pub(crate) const BACKGROUND_COLOUR: &str = "pdf-background-colour-text";
    /// Drawn outside the page's `MediaBox`.
    pub(crate) const OUTSIDE_MEDIA_BOX: &str = "pdf-outside-media-box";
    /// Drawn inside `MediaBox` but outside `CropBox`, so a viewer crops it away.
    pub(crate) const OUTSIDE_CROP_BOX: &str = "pdf-outside-crop-box";
    /// Drawn at a size no rendering can show.
    pub(crate) const DEGENERATE_SIZE: &str = "pdf-degenerate-font-size";

    /// Every token above, for the test that holds the set against the fixture
    /// corpus. Test-only: nothing in the library enumerates the mechanisms, it
    /// names the one it found.
    #[cfg(test)]
    pub(crate) const ALL: &[&str] = &[
        INVISIBLE_RENDER_MODE,
        BACKGROUND_COLOUR,
        OUTSIDE_MEDIA_BOX,
        OUTSIDE_CROP_BOX,
        DEGENERATE_SIZE,
    ];
}

/// A run of text the content stream draws in a way that hides it, with the
/// mechanism that hid it.
///
/// Owned rather than borrowed because the text is *decoded* — it does not exist
/// as a slice of the input anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Concealed {
    /// The stable token, or `None` when the mechanism is ordinary document
    /// construction and only the content is suspect — see the module's note on
    /// searchable scans.
    pub(crate) mechanism: Option<&'static str>,
    /// A short human-readable description. Never the concealed text itself.
    pub(crate) detail: String,
    /// The decoded text of the run.
    pub(crate) text: String,
}

impl Concealed {
    /// Borrow as the type [`crate::screen`] takes.
    pub(crate) fn as_region(&self) -> ConcealedText<'_> {
        ConcealedText {
            mechanism: self.mechanism,
            detail: &self.detail,
            text: &self.text,
        }
    }
}

/// Read `bytes` as a PDF and return every text run it conceals.
///
/// An empty result means the document draws nothing hidden **or could not be
/// parsed**; the two are not distinguished, because a document this reader
/// cannot parse is one it has no findings about, and inventing a third outcome
/// would put a parser's health into a security verdict. The flat text extraction
/// this runs beside already reports its own failures as `meta.extract` (#907),
/// which is where a reader looks to tell "nothing concealed" from "nothing
/// read".
///
/// Panic-guarded for the reason `pdf_text` is: `lopdf` can panic on a
/// malformed document, and a bad PDF must degrade to "nothing found" rather than
/// abort a whole sync.
pub(crate) fn concealed_runs(bytes: &[u8]) -> Vec<Concealed> {
    let owned = bytes.to_vec();
    std::panic::catch_unwind(move || scan(&owned)).unwrap_or_default()
}

/// The body of [`concealed_runs`], outside the panic guard.
fn scan(bytes: &[u8]) -> Vec<Concealed> {
    let Ok(doc) = Document::load_mem(bytes) else {
        return Vec::new();
    };
    let mut out: Vec<Concealed> = Vec::new();
    let mut budget = MAX_CONCEALED_CHARS;
    // `get_pages` is a `BTreeMap`, so page order is the document's, not a hash
    // order — which is what keeps this a pure function of the bytes.
    for (_number, page_id) in doc.get_pages() {
        if budget == 0 {
            break;
        }
        scan_page(&doc, page_id, &mut out, &mut budget);
    }
    out
}

/// Walk one page's content stream, appending what it conceals.
fn scan_page(doc: &Document, page_id: ObjectId, out: &mut Vec<Concealed>, budget: &mut usize) {
    let Ok(content) = doc.get_and_decode_page_content(page_id) else {
        return;
    };
    let media = page_box(doc, page_id, b"MediaBox").unwrap_or(Rect::LETTER);
    // A page with no `CropBox` is cropped to its `MediaBox` (PDF 32000-1 §7.7.3.3),
    // so the two tests coincide and only the MediaBox one can fire.
    let crop = page_box(doc, page_id, b"CropBox").unwrap_or(media);
    let fonts: PageFonts<'_> = doc.get_page_fonts(page_id).unwrap_or_default();
    let images = image_names(doc, page_id);
    let spaces = page_colour_spaces(doc, page_id);

    let mut gs = GraphicsState::default();
    let mut stack: Vec<GraphicsState> = Vec::new();
    let mut text = TextState::default();
    let mut painted: Vec<Painted> = Vec::new();
    let mut path: Vec<Rect> = Vec::new();

    for op in &content.operations {
        let operands = &op.operands;
        match op.operator.as_str() {
            "q" => stack.push(gs.clone()),
            "Q" => gs = stack.pop().unwrap_or_default(),
            "cm" => {
                if let Some(m) = matrix(operands) {
                    gs.ctm = m.then(gs.ctm);
                }
            }
            // These three set the space *and* the colour in one operator, so
            // they also re-establish that the next `sc`/`scn` is readable.
            "g" | "G" => set_colour(&mut gs, op.operator.as_str(), gray(operands)),
            "rg" | "RG" => set_colour(&mut gs, op.operator.as_str(), rgb(operands)),
            "k" | "K" => set_colour(&mut gs, op.operator.as_str(), cmyk(operands)),
            "cs" | "CS" => {
                let space = colour_space(operands, &spaces);
                if op.operator == "cs" {
                    gs.fill_space = space;
                    // §8.6.8: selecting a space resets the colour to that
                    // space's initial value. Unknown for anything but a device
                    // space, and unknown never stands in for a value.
                    gs.fill = (space == ColourSpace::Device).then_some([0.0, 0.0, 0.0]);
                } else {
                    gs.stroke_space = space;
                    gs.stroke = (space == ColourSpace::Device).then_some([0.0, 0.0, 0.0]);
                }
            }
            // `sc`/`scn` set a colour in whatever space `cs` selected. Numeric
            // operands are read by arity; a pattern or a named colour leaves the
            // fill **unknown**, which suppresses the colour test rather than
            // guessing at it.
            "sc" | "scn" | "SC" | "SCN" => {
                let space = if op.operator.starts_with(char::is_lowercase) {
                    gs.fill_space
                } else {
                    gs.stroke_space
                };
                set_colour(&mut gs, op.operator.as_str(), components(space, operands));
            }
            "re" => {
                if let Some(r) = rect(operands, gs.ctm) {
                    path.push(r);
                }
            }
            // Every fill operator. A stroke-only `S`/`s` paints a hairline, not a
            // region, so it does not cover what is behind it and is not recorded.
            "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                let paint = gs.fill.map_or(Paint::Opaque, Paint::Solid);
                painted.extend(path.drain(..).map(|rect| Painted { rect, paint }));
            }
            "n" | "S" | "s" => path.clear(),
            "Do" => {
                // An image covers its rectangle in colours this reader does not
                // read. A form XObject is not followed — its own content stream is
                // a separate scan this deliberately does not recurse into.
                if operands
                    .first()
                    .and_then(|o| o.as_name().ok())
                    .is_some_and(|name| images.iter().any(|n| n == name))
                {
                    painted.push(Painted {
                        rect: Rect::unit().transform(gs.ctm),
                        paint: Paint::Opaque,
                    });
                }
            }
            // `BT` resets the text and text-line matrices and **nothing else**
            // (§9.4.1); the font, size, leading and rendering mode persist
            // across text objects because they are graphics state.
            "BT" => text = TextState::default(),
            "Tf" | "Tr" | "TL" | "Tz" | "Ts" => {
                gs.text.apply(op.operator.as_str(), operands);
            }
            "Tm" | "Td" | "TD" | "T*" => {
                text.apply(op.operator.as_str(), operands, &mut gs.text.leading);
            }
            "Tj" | "TJ" | "'" | "\"" => {
                if op.operator == "'" || op.operator == "\"" {
                    text.next_line(0.0, -gs.text.leading);
                }
                let strings = match op.operator.as_str() {
                    // `"` is `aw Tw ac Tc T* Tj`: the string is the third operand.
                    "\"" => operands.get(2).map_or(&[][..], std::slice::from_ref),
                    _ => operands.as_slice(),
                };
                inspect_run(
                    &Run {
                        doc,
                        gs: &gs,
                        text: &text,
                        media,
                        crop,
                        painted: &painted,
                        fonts: &fonts,
                    },
                    strings,
                    out,
                    budget,
                );
            }
            // Every other operator is read past, and two are worth naming
            // because their absence looks like an oversight:
            //
            // * `ET` closes a text object whose state the next `BT` resets;
            // * `gs`, `W` and `W*` are the alpha and clipping this module
            //   declares as non-goals.
            _ => {}
        }
    }
}

/// Everything one text-showing operator is judged against.
struct Run<'a> {
    doc: &'a Document,
    gs: &'a GraphicsState,
    text: &'a TextState,
    media: Rect,
    crop: Rect,
    painted: &'a [Painted],
    fonts: &'a PageFonts<'a>,
}

/// Judge one text-showing operator, appending a [`Concealed`] per mechanism it
/// trips.
///
/// Several can fire on one run — text can be both off-page and invisibly small —
/// and each is recorded, because `meta.screen` names *what was found*, not a
/// single best explanation.
fn inspect_run(run: &Run<'_>, strings: &[Object], out: &mut Vec<Concealed>, budget: &mut usize) {
    // The full text rendering matrix of §9.4.4, not `Tm · CTM`: the text-state
    // parameters `Tfs`, `Th` and `Ts` are *part of the transform*, and reading
    // the origin without them misses a glyph that `Ts` has lifted off the page.
    let trm = run
        .gs
        .text
        .rendering_matrix()
        .then(run.text.tm)
        .then(run.gs.ctm);
    let (x, y) = trm.apply(0.0, 0.0);
    // Both axes. A glyph is illegible if *either* collapses — `0 Tz` leaves the
    // vertical scale at a perfectly ordinary 12 pt while the glyph occupies no
    // width at all.
    let effective = trm.vertical_scale().min(trm.horizontal_scale());
    let behind = topmost(run.painted, x, y);

    let mut found: Vec<(&'static str, String)> = Vec::new();
    let mut unreported: Option<String> = None;

    // Modes 3 and 7 paint nothing at all. 7 adds to the clipping path, which is
    // not a mark on the page either; listing both is the difference between
    // detecting the mechanism and detecting one spelling of it.
    if matches!(run.gs.text.render_mode, 3 | 7) {
        if matches!(behind, Paint::Opaque) {
            unreported = Some(
                "invisible text layer over a page image (the ordinary searchable-scan shape)"
                    .to_owned(),
            );
        } else {
            found.push((
                mechanism::INVISIBLE_RENDER_MODE,
                format!("text rendering mode {}", run.gs.text.render_mode),
            ));
        }
    } else if let Some(inks) = ink_colours(run.gs)
        && let Paint::Solid(background) = behind
        && inks.iter().all(|ink| indistinguishable(*ink, background))
    {
        found.push((
            mechanism::BACKGROUND_COLOUR,
            "text colour indistinguishable from what is painted behind it".to_owned(),
        ));
    }

    // MediaBox first: text outside it is outside CropBox too, and reporting both
    // would say the same thing twice.
    if !run.media.contains(x, y) {
        found.push((
            mechanism::OUTSIDE_MEDIA_BOX,
            "glyph origin outside the page MediaBox".to_owned(),
        ));
    } else if !run.crop.contains(x, y) {
        found.push((
            mechanism::OUTSIDE_CROP_BOX,
            "glyph origin outside the page CropBox".to_owned(),
        ));
    }

    // `run.gs.text.font.is_some()` is exactly "a `Tf` has executed in this text
    // object": the operator sets the font and the size together. Without it a
    // malformed document that shows text with no font selected would be reported
    // as concealing it at the default size of zero, which is a claim about the
    // parser's state rather than about the page.
    if run.gs.text.font.is_some() && effective.abs() < MIN_LEGIBLE_SIZE {
        found.push((
            mechanism::DEGENERATE_SIZE,
            format!("effective text size {effective:.4} pt"),
        ));
    }

    if found.is_empty() && unreported.is_none() {
        return;
    }
    let decoded = decode_run(run.doc, run.fonts, run.gs.text.font.as_deref(), strings);
    // A run with no reportable mechanism exists only to have its *content*
    // scanned, so one that decoded to nothing has neither half and is dropped.
    // A run *with* a mechanism is kept whatever it decoded to: a subset font
    // with no usable encoding yields nothing legible, and reporting the class
    // with no text still quarantines the document. There is simply nothing left
    // to match a directive against — the safe failure direction, and the reason
    // this module claims `block` is *reachable* rather than universal.
    if found.is_empty() && decoded.is_empty() {
        return;
    }
    let take = decoded.chars().count().min(*budget);
    let text: String = decoded.chars().take(take).collect();
    *budget -= take;

    for (mechanism, detail) in found {
        out.push(Concealed {
            mechanism: Some(mechanism),
            detail,
            text: text.clone(),
        });
    }
    if let Some(detail) = unreported {
        out.push(Concealed {
            mechanism: None,
            detail,
            text,
        });
    }
}

/// Every colour this run actually puts on the page, or `None` when one of them
/// is in a space this reader does not read.
///
/// The text rendering mode decides which (PDF 32000-1 §9.3.6), and getting it
/// wrong is wrong in both directions:
///
/// | mode | paints | ink |
/// |---|---|---|
/// | 0, 4 | fill | the fill colour |
/// | 1, 5 | **stroke** | the **stroking** colour |
/// | 2, 6 | fill and stroke | **both** |
/// | 3, 7 | nothing | handled before this is reached |
///
/// Modes 1 and 5 are why the stroking colour is tracked at all: they put no
/// fill on the page, so reading the fill there asks about ink that was never
/// laid down.
///
/// Modes 2 and 6 need *both* to match, and that direction matters just as much:
/// a glyph filled white and stroked black is perfectly visible, and reporting
/// it on the strength of the fill alone would be a false positive on every
/// outlined heading.
///
/// `None` anywhere in the set suppresses the check rather than guessing — an
/// unknown colour is not evidence of concealment.
fn ink_colours(gs: &GraphicsState) -> Option<Vec<[f64; 3]>> {
    match gs.text.render_mode {
        0 | 4 => gs.fill.map(|c| vec![c]),
        1 | 5 => gs.stroke.map(|c| vec![c]),
        2 | 6 => match (gs.fill, gs.stroke) {
            (Some(fill), Some(stroke)) => Some(vec![fill, stroke]),
            _ => None,
        },
        _ => None,
    }
}

/// Whether two colours are closer than an 8-bit render can separate.
fn indistinguishable(a: [f64; 3], b: [f64; 3]) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= COLOUR_TOLERANCE)
}

/// What is painted at `(x, y)`, topmost first, or the page's own white where
/// nothing is.
///
/// White is an **assumption, and the only one available**: a PDF page has no
/// background colour, and an unpainted page is transparent — "white" is a
/// property of the paper it is imagined on. Every fixture here paints its
/// background explicitly for that reason, so the assumption is not what the
/// measurement rests on.
fn topmost(painted: &[Painted], x: f64, y: f64) -> Paint {
    painted
        .iter()
        .rev()
        .find(|p| p.rect.contains(x, y))
        .map_or(Paint::Solid([1.0, 1.0, 1.0]), |p| p.paint)
}

/// The font dictionaries a page's `/Resources` declares, by resource name.
///
/// Kept as dictionaries rather than resolved `Encoding` values because
/// `Encoding<'a>` borrows the document, and the walker would have to carry that
/// lifetime through every state struct to hold one. Re-resolving per run costs a
/// `BTreeMap` hit and a small dictionary read.
type PageFonts<'a> = BTreeMap<Vec<u8>, &'a Dictionary>;

/// Decode one text-showing operator's operands through the current font's
/// encoding.
///
/// `TJ`'s numeric elements are kerning adjustments, not characters, and are
/// skipped. A font with no resolvable encoding yields **nothing** rather than
/// raw bytes: handing undecoded glyph indices to the directive matcher would be
/// matching English patterns against noise, which can only produce false
/// positives with no compensating detection.
fn decode_run(
    doc: &Document,
    fonts: &PageFonts<'_>,
    font: Option<&[u8]>,
    strings: &[Object],
) -> String {
    let Some(dict) = font.and_then(|name| fonts.get(name)) else {
        return String::new();
    };
    let Ok(encoding) = dict.get_font_encoding(doc) else {
        return String::new();
    };
    let mut out = String::new();
    let push = |bytes: &[u8], out: &mut String| {
        if let Ok(text) = Document::decode_text(&encoding, bytes) {
            out.push_str(&text);
        }
    };
    for object in strings {
        match object {
            Object::String(bytes, _) => push(bytes, &mut out),
            Object::Array(items) => {
                for item in items {
                    if let Object::String(bytes, _) = item {
                        push(bytes, &mut out);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Resolve a `cs`/`CS` operand to a [`ColourSpace`].
///
/// The operand is either a device space by name, or a key into the page's
/// `/Resources /ColorSpace` — and resolving that alias matters in the opposite
/// direction from the Separation case: a document that writes `/CS0 cs` where
/// `CS0` *is* `/DeviceRGB` is ordinary, and treating the alias as unreadable
/// would suppress the colour check on it and miss real white-on-white.
fn colour_space(operands: &[Object], spaces: &BTreeMap<Vec<u8>, ColourSpace>) -> ColourSpace {
    let Some(name) = operands.first().and_then(|o| o.as_name().ok()) else {
        return ColourSpace::Other;
    };
    if is_device_space(name) {
        return ColourSpace::Device;
    }
    spaces.get(name).copied().unwrap_or(ColourSpace::Other)
}

/// The three spaces whose components `components` can read.
fn is_device_space(name: &[u8]) -> bool {
    matches!(
        name,
        b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" | b"G" | b"RGB" | b"CMYK"
    )
}

/// The page's `/Resources /ColorSpace` entries, classified.
///
/// Only the aliases that resolve to a device space are worth recording; an
/// entry this does not list is [`ColourSpace::Other`] by
/// [`colour_space`]'s fallback, which is the safe direction.
fn page_colour_spaces(doc: &Document, page_id: ObjectId) -> BTreeMap<Vec<u8>, ColourSpace> {
    let Ok((Some(resources), _)) = doc.get_page_resources(page_id) else {
        return BTreeMap::new();
    };
    let Ok(declared) = resources.get(b"ColorSpace").and_then(Object::as_dict) else {
        return BTreeMap::new();
    };
    declared
        .iter()
        .map(|(name, object)| {
            // A device space may be named directly or wrapped in a one-element
            // array; anything longer is a parameterised space (`Separation`,
            // `Indexed`, `ICCBased`, `Lab`) whose components are not device
            // components.
            let resolved = match object {
                Object::Name(n) if is_device_space(n) => ColourSpace::Device,
                Object::Array(items) if items.len() == 1 => match &items[0] {
                    Object::Name(n) if is_device_space(n) => ColourSpace::Device,
                    _ => ColourSpace::Other,
                },
                _ => ColourSpace::Other,
            };
            (name.clone(), resolved)
        })
        .collect()
}

/// The image `XObject` names a page declares.
///
/// Only images: a form `XObject`'s own content stream is not walked, so recording
/// it as page coverage would claim an opaque rectangle this reader never looked
/// inside.
fn image_names(doc: &Document, page_id: ObjectId) -> Vec<Vec<u8>> {
    let Ok((Some(resources), _)) = doc.get_page_resources(page_id) else {
        return Vec::new();
    };
    let Ok(xobjects) = resources.get(b"XObject").and_then(Object::as_dict) else {
        return Vec::new();
    };
    xobjects
        .iter()
        .filter(|(_, object)| {
            let dict = match object {
                Object::Reference(id) => doc
                    .get_object(*id)
                    .and_then(Object::as_stream)
                    .map(|s| &s.dict),
                Object::Stream(stream) => Ok(&stream.dict),
                _ => return false,
            };
            dict.and_then(|d| d.get(b"Subtype"))
                .and_then(Object::as_name)
                .is_ok_and(|subtype| subtype == b"Image")
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// A page's `MediaBox` or `CropBox`, following `/Parent` for an inherited one.
///
/// Inheritance is the reason this is not a one-line `get`: both are inheritable
/// page-tree attributes, and a document that sets `MediaBox` once on `/Pages`
/// and never on a page is entirely ordinary. Reading only the page dictionary
/// would fall back to the default page size and report a correctly-placed
/// document as drawing off-page.
fn page_box(doc: &Document, page_id: ObjectId, key: &[u8]) -> Option<Rect> {
    let mut id = page_id;
    // Bounded: a page tree deeper than this is malformed, and following
    // `/Parent` without a bound is how a cyclic one becomes a hang.
    for _ in 0..32 {
        let dict: &Dictionary = doc.get_dictionary(id).ok()?;
        if let Ok(array) = dict.get(key).and_then(Object::as_array) {
            let values: Vec<f64> = array.iter().filter_map(number).collect();
            if let [x0, y0, x1, y1] = values[..] {
                return Some(Rect::normalised(x0, y0, x1, y1));
            }
        }
        id = dict.get(b"Parent").and_then(Object::as_reference).ok()?;
    }
    None
}

/// The current transformation matrix and fill colour.
#[derive(Debug, Clone)]
struct GraphicsState {
    ctm: Matrix,
    /// `None` when the colour is in a space this reader does not read — a
    /// pattern, a separation, an indexed space. Unknown suppresses the colour
    /// test; it never stands in for a value.
    fill: Option<[f64; 3]>,
    /// The **stroking** colour, on the same terms as [`Self::fill`].
    ///
    /// Read because text rendering modes 1 and 5 *stroke glyphs and do not fill
    /// them*, so for those modes the stroking colour is the only ink on the
    /// page — and `1 Tr` with the stroking colour set to the background is
    /// white-on-white concealment spelled one operator over. Discarding it, as
    /// this reader did until the #912 review, left the class it claims to
    /// detect trivially bypassable.
    stroke: Option<[f64; 3]>,
    /// The space [`Self::fill`]'s `sc`/`scn` operands are read in.
    fill_space: ColourSpace,
    /// The same for [`Self::stroke`].
    stroke_space: ColourSpace,
    /// The **text state**, which is part of the graphics state and therefore
    /// saved and restored by `q`/`Q` (PDF 32000-1 §9.3).
    ///
    /// It lives here rather than in [`TextState`] because that is what the
    /// specification says, and getting it wrong is wrong in both directions: a
    /// stream that sets `3 Tr` inside a `q` and then restores leaves a reader
    /// that kept the mode flagging **visible** prose as concealed, and one that
    /// sets `3 Tr`, saves, sets `0 Tr`, restores, then shows text is a
    /// concealment a reader that reset the mode never sees. Fixtures 15 and 16
    /// are those two cases.
    text: TextParameters,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            // Black, in `DeviceGray` — the specification's initial value for
            // both.
            fill: Some([0.0, 0.0, 0.0]),
            stroke: Some([0.0, 0.0, 0.0]),
            // `DeviceGray`, per §8.6.8's initial colour space.
            fill_space: ColourSpace::Device,
            stroke_space: ColourSpace::Device,
            text: TextParameters::default(),
        }
    }
}

/// The text-state parameters this reader uses: the selected font and size, the
/// leading, and the rendering mode.
///
/// Part of the **graphics** state (see [`GraphicsState::text`]), so `q`/`Q` save
/// and restore them and `BT` does **not** reset them — `BT` initialises only the
/// text and text-line matrices (§9.4.1), which is why those two live in
/// [`TextState`] instead.
#[derive(Debug, Clone)]
struct TextParameters {
    /// `Tf`'s size operand — **not** the size in points. See
    /// [`TextParameters::rendering_matrix`].
    size: f64,
    /// `Tz`, as a multiplier rather than the operator's percentage.
    ///
    /// Modelled because `0 Tz` collapses every glyph to zero width: invisible
    /// text by a mechanism that leaves `Tf` reading an ordinary 12 pt.
    horizontal_scale: f64,
    /// `Ts`, the text rise, in unscaled text-space units.
    ///
    /// Modelled because it translates the glyph away from the origin `Td` put
    /// it at — so a large rise puts text off the page while the positioning
    /// operators claim it is in the middle of it.
    rise: f64,
    leading: f64,
    render_mode: i64,
    font: Option<Vec<u8>>,
}

impl Default for TextParameters {
    fn default() -> Self {
        Self {
            size: 0.0,
            // 100 % — the specification's initial value, as a multiplier.
            horizontal_scale: 1.0,
            rise: 0.0,
            leading: 0.0,
            render_mode: 0,
            font: None,
        }
    }
}

impl TextParameters {
    /// The text-space parameter matrix of PDF 32000-1 §9.4.4:
    ///
    /// ```text
    /// | Tfs x Th   0     0 |
    /// | 0          Tfs   0 |
    /// | 0          Ts    1 |
    /// ```
    ///
    /// Composed with the text matrix and the CTM, this is the transform that
    /// actually puts a glyph on the page — which is why the reader builds it
    /// rather than treating `Tf`'s operand as a size in points and the `Td`
    /// origin as a position.
    ///
    /// # Why the whole matrix rather than three more tests
    ///
    /// Two of these parameters are concealment mechanisms in their own right,
    /// and both were live before this existed (#912 review):
    ///
    /// * **`Ts`** translates the glyph away from where `Td` put it, so a rise
    ///   of 100 000 draws off the page while every positioning operator in the
    ///   stream reads as ordinary. A reader that took the origin from `Tm·CTM`
    ///   alone sees a glyph in the middle of the page.
    /// * **`Th`** scales glyphs horizontally, and `0 Tz` collapses them to
    ///   nothing while `Tf` still reads 12 pt. A reader that measured only the
    ///   vertical scale sees an ordinary line of text.
    ///
    /// Both fall out of composing the matrix the specification defines. Adding
    /// a test per parameter instead is how the *next* one gets missed: the
    /// question "what else moves a glyph?" has an answer in Table 103, and it
    /// is this matrix.
    fn rendering_matrix(&self) -> Matrix {
        Matrix {
            a: self.size * self.horizontal_scale,
            b: 0.0,
            c: 0.0,
            d: self.size,
            e: 0.0,
            f: self.rise,
        }
    }

    /// Apply one text-state operator, or report that this is not one.
    fn apply(&mut self, operator: &str, operands: &[Object]) -> bool {
        match operator {
            "Tf" => {
                self.font = operands
                    .first()
                    .and_then(|o| o.as_name().ok())
                    .map(<[u8]>::to_vec);
                self.size = operands.get(1).and_then(number).unwrap_or(0.0);
            }
            // The operand is an integer by the specification, so this reads it as
            // one rather than rounding a float: a `Tr` operand that is not a
            // whole number is malformed, and mode 0 (fill) is the value the
            // specification itself defaults to.
            "Tr" => self.render_mode = operands.first().and_then(integer).unwrap_or(0),
            "TL" => self.leading = operands.first().and_then(number).unwrap_or(0.0),
            // The operand is a percentage; the initial value is 100.
            "Tz" => {
                self.horizontal_scale = operands.first().and_then(number).unwrap_or(100.0) / 100.0;
            }
            "Ts" => self.rise = operands.first().and_then(number).unwrap_or(0.0),
            _ => return false,
        }
        true
    }
}

/// The text object state `BT` resets.
#[derive(Debug, Clone)]
struct TextState {
    tm: Matrix,
    /// The text *line* matrix: what `Td` and `T*` translate from, and what `Tm`
    /// resets alongside `tm`. Tracking only `tm` makes every line after the
    /// first land wherever the previous one started.
    tlm: Matrix,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            tm: Matrix::IDENTITY,
            tlm: Matrix::IDENTITY,
        }
    }
}

impl TextState {
    /// Apply one text-**positioning** operator. `TD` also sets the leading,
    /// which lives in the graphics state, so that is passed in.
    fn apply(&mut self, operator: &str, operands: &[Object], leading: &mut f64) {
        let first = operands.first().and_then(number).unwrap_or(0.0);
        let second = operands.get(1).and_then(number).unwrap_or(0.0);
        match operator {
            "Tm" => {
                if let Some(m) = matrix(operands) {
                    self.tm = m;
                    self.tlm = m;
                }
            }
            "Td" => self.next_line(first, second),
            // `TD` is `-ty TL` followed by `tx ty Td`.
            "TD" => {
                *leading = -second;
                self.next_line(first, second);
            }
            "T*" => self.next_line(0.0, -*leading),
            _ => {}
        }
    }

    /// `Td`: translate the line matrix and restart the text matrix from it.
    fn next_line(&mut self, tx: f64, ty: f64) {
        self.tlm = Matrix::translation(tx, ty).then(self.tlm);
        self.tm = self.tlm;
    }
}

/// A 2-D affine transform, in PDF's `[a b c d e f]` order.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Matrix {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Matrix {
    const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    const fn translation(tx: f64, ty: f64) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
    }

    /// `self` followed by `outer` — matrix multiplication in PDF's order, where
    /// a `cm` premultiplies the existing CTM.
    fn then(self, outer: Self) -> Self {
        Self {
            a: self.a * outer.a + self.b * outer.c,
            b: self.a * outer.b + self.b * outer.d,
            c: self.c * outer.a + self.d * outer.c,
            d: self.c * outer.b + self.d * outer.d,
            e: self.e * outer.a + self.f * outer.c + outer.e,
            f: self.e * outer.b + self.f * outer.d + outer.f,
        }
    }

    fn apply(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// How far this transform stretches a unit step up the y axis — the factor a
    /// `Tf` size is multiplied by to get points on the page.
    ///
    /// `(c*c + d*d).sqrt()` rather than `f64::hypot`: `sqrt` is an IEEE-754
    /// exact operation and identical on every platform, while `hypot` is a libm
    /// call that need not be. A size threshold that moved by a ulp between macOS
    /// and Linux would make this reader's verdict platform-dependent, and a
    /// verdict that is not a pure function of the bytes cannot be a graph fact.
    fn vertical_scale(self) -> f64 {
        (self.c * self.c + self.d * self.d).sqrt()
    }

    /// The same, across. `0 Tz` drives this to zero while
    /// [`Self::vertical_scale`] stays at an ordinary 12.
    fn horizontal_scale(self) -> f64 {
        (self.a * self.a + self.b * self.b).sqrt()
    }
}

/// An axis-aligned rectangle in default user space.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Rect {
    /// US Letter — the fallback when a document declares no `MediaBox` anywhere
    /// in the page tree, which is malformed but not rare.
    const LETTER: Self = Self {
        x0: 0.0,
        y0: 0.0,
        x1: 612.0,
        y1: 792.0,
    };

    /// PDF rectangles may be given by any two opposite corners, so the operands
    /// are not ordered.
    fn normalised(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    /// The unit square an image `XObject` is always drawn into before the CTM
    /// scales it.
    const fn unit() -> Self {
        Self {
            x0: 0.0,
            y0: 0.0,
            x1: 1.0,
            y1: 1.0,
        }
    }

    /// The axis-aligned bounding box of this rectangle under `m`. A rotated
    /// rectangle's bounding box over-covers, which makes coverage a *suppressor*
    /// that errs toward silence — the safe direction for a check whose positives
    /// are findings.
    fn transform(self, m: Matrix) -> Self {
        let corners = [
            m.apply(self.x0, self.y0),
            m.apply(self.x1, self.y0),
            m.apply(self.x0, self.y1),
            m.apply(self.x1, self.y1),
        ];
        let xs = corners.map(|(x, _)| x);
        let ys = corners.map(|(_, y)| y);
        Self {
            x0: xs.iter().copied().fold(f64::INFINITY, f64::min),
            y0: ys.iter().copied().fold(f64::INFINITY, f64::min),
            x1: xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            y1: ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        }
    }

    fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
}

/// The colour space a `sc`/`scn` operand is to be read in.
///
/// # Why this is tracked rather than assumed
///
/// `sc`/`scn` carry bare numbers whose meaning is whatever the preceding
/// `cs`/`CS` selected, and this reader used to read them **by arity alone** —
/// one number as grey, three as RGB, four as CMYK. In a device space that is
/// right. In a `/Separation` space it is backwards: `1 scn` is *full spot ink*,
/// as dark as that colourant goes, and reading it as `[1, 1, 1]` calls it white.
///
/// That is not an exotic failure. Spot colours are ordinary in publisher-set
/// PDFs, so a paper with a spot-coloured rule or heading on a white page would
/// have been reported as **white-on-white concealment** — a false positive on
/// exactly the corpus this module exists to screen, in the direction that
/// quarantines honest documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColourSpace {
    /// One of the three device spaces, where the arity read is correct.
    Device,
    /// Anything else — `Separation`, `Indexed`, `Lab`, `Pattern`, or a resource
    /// name that does not resolve to a device space. The components are not
    /// device components, so the colour is **unknown** rather than guessed.
    Other,
}

/// What a painted region puts on the page.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Paint {
    /// A solid fill in a colour this reader read.
    Solid([f64; 3]),
    /// An image, or a fill in a colour space this reader does not read — the
    /// colour behind a glyph here is unknown, so the colour test is skipped
    /// rather than guessed.
    Opaque,
}

/// A painted region, in the order the content stream painted it.
#[derive(Debug, Clone, Copy)]
struct Painted {
    rect: Rect,
    paint: Paint,
}

/// A numeric operand, as `f64`. Integers and reals both.
fn number(object: &Object) -> Option<f64> {
    match object {
        // Lossy past 2^53, and that cannot matter here: a PDF coordinate space
        // is bounded by the format at +/-3.4e38 with far less precision than
        // this, and a page dimension near 2^53 points is a corrupt file, not a
        // measurement.
        #[expect(
            clippy::cast_precision_loss,
            reason = "PDF coordinates never approach 2^53; see above"
        )]
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(f64::from(*r)),
        _ => None,
    }
}

/// An integer operand. `Tr` is the only operator here whose operand is defined
/// as one, and reading it as an integer rather than truncating a float keeps a
/// malformed `3.7 Tr` from silently becoming the invisible mode 3.
fn integer(object: &Object) -> Option<i64> {
    match object {
        Object::Integer(i) => Some(*i),
        _ => None,
    }
}

/// `cm` and `Tm` operands as a matrix.
fn matrix(operands: &[Object]) -> Option<Matrix> {
    let values: Vec<f64> = operands.iter().filter_map(number).collect();
    match values[..] {
        [scale_x, shear_x, shear_y, scale_y, offset_x, offset_y] => Some(Matrix {
            a: scale_x,
            b: shear_x,
            c: shear_y,
            d: scale_y,
            e: offset_x,
            f: offset_y,
        }),
        _ => None,
    }
}

/// `re` operands as a rectangle in default user space.
fn rect(operands: &[Object], ctm: Matrix) -> Option<Rect> {
    let values: Vec<f64> = operands.iter().filter_map(number).collect();
    match values[..] {
        [x, y, width, height] => Some(Rect::normalised(x, y, x + width, y + height).transform(ctm)),
        _ => None,
    }
}

/// Route a colour operator to the fill or the stroke by its case: PDF spells
/// the non-stroking operators in lower case and the stroking ones in upper.
///
/// Both are kept. Which one is ink depends on the text rendering mode — see
/// [`ink_colours`] — and reading only the fill made modes 1 and 5 invisible to
/// the colour check entirely.
fn set_colour(gs: &mut GraphicsState, operator: &str, value: Option<[f64; 3]>) {
    if operator.chars().next().is_some_and(char::is_lowercase) {
        gs.fill = value;
    } else {
        gs.stroke = value;
    }
}

fn gray(operands: &[Object]) -> Option<[f64; 3]> {
    match operands.iter().filter_map(number).collect::<Vec<_>>()[..] {
        [g] => Some([g, g, g]),
        _ => None,
    }
}

fn rgb(operands: &[Object]) -> Option<[f64; 3]> {
    match operands.iter().filter_map(number).collect::<Vec<_>>()[..] {
        [r, g, b] => Some([r, g, b]),
        _ => None,
    }
}

/// CMYK, converted by the naive `1 - min(1, x + k)` rule.
///
/// Good enough for the one question asked of it — *is this the same colour as
/// what is behind it?* — because the same conversion is applied to both sides.
/// It is not a colour-managed conversion and is not used for anything else.
fn cmyk(operands: &[Object]) -> Option<[f64; 3]> {
    match operands.iter().filter_map(number).collect::<Vec<_>>()[..] {
        [c, m, y, k] => Some([
            1.0 - (c + k).min(1.0),
            1.0 - (m + k).min(1.0),
            1.0 - (y + k).min(1.0),
        ]),
        _ => None,
    }
}

/// `sc`/`scn` operands, read by arity — but **only in a device space**, where
/// arity is enough to know what the numbers mean. See [`ColourSpace`] for the
/// false positive this guard exists to prevent.
///
/// A pattern name, any other arity, or any non-device space leaves the colour
/// unknown, which suppresses the colour check rather than guessing at it.
fn components(space: ColourSpace, operands: &[Object]) -> Option<[f64; 3]> {
    if space != ColourSpace::Device {
        return None;
    }
    match operands.iter().filter_map(number).collect::<Vec<_>>()[..] {
        [g] => Some([g, g, g]),
        [r, g, b] => Some([r, g, b]),
        [c, m, y, k] => Some([
            1.0 - (c + k).min(1.0),
            1.0 - (m + k).min(1.0),
            1.0 - (y + k).min(1.0),
        ]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Verdict;

    /// The committed fixtures, by `include_bytes!` rather than by path.
    ///
    /// Compile-time on purpose, for the reason `audio_fixtures.rs` records about
    /// its own shared artefact: a renamed or deleted fixture is then a build
    /// failure here, not a test that silently stops covering a class at run time.
    macro_rules! fixtures {
        ($($name:literal),+ $(,)?) => {
            &[$(($name, include_bytes!(concat!("../tests/fixtures/pdf/", $name, ".pdf")).as_slice())),+]
        };
    }

    /// Every fixture, the verdict the *whole* screen reaches on it, and the
    /// classes recorded on the node.
    ///
    /// This is the table #813 asks for, and it is asserted rather than described.
    /// The left half is the point of the issue — six of the seven concealment
    /// fixtures reach `Block`, which **no PDF could reach before this module
    /// existed**, because the screen's block rule needs concealment and
    /// direction together and PDF concealment was structurally invisible to it.
    ///
    /// The right half is what stops it being a detector that flags everything:
    /// seven pages that carry no concealment — five matched controls and three
    /// shapes that are ordinary in a real corpus — and all of them `Pass`.
    const CORPUS: &[(&str, &[u8])] = fixtures![
        "01-clean-control",
        "02-white-on-white",
        "02-white-on-white-control",
        "03-outside-mediabox",
        "03-outside-mediabox-control",
        "03b-outside-cropbox",
        "03b-outside-cropbox-control",
        "04-degenerate-size",
        "04-degenerate-size-control",
        "05-render-mode-3",
        "05-render-mode-3-control",
        "06-lying-layer",
        "06-lying-layer-control",
        "07-under-image",
        "07-under-image-control",
        "11-concealed-note",
        "11-concealed-note-control",
        "12-inherited-boxes",
        "12-inherited-boxes-control",
        "13-ctm-shrunk-text",
        "13-ctm-shrunk-text-control",
        "16-render-mode-across-q",
        "16-render-mode-across-q-control",
        "17-degenerate-size-across-q",
        "17-degenerate-size-across-q-control",
        "18-lying-layer-across-q",
        "18-lying-layer-across-q-control",
        "19-text-rise-off-page",
        "19-text-rise-off-page-control",
        "20-zero-horizontal-scale",
        "20-zero-horizontal-scale-control",
        "21-stroke-only-background",
        "21-stroke-only-background-control",
        "23-aliased-device-space",
        "23-aliased-device-space-control",
        "08-scaled-text-matrix",
        "09-white-text-over-image",
        "10-searchable-scan",
        "14-scaled-placement",
        "15-restored-render-mode",
        "22-outlined-heading",
        "24-separation-spot-colour",
    ];

    /// What each fixture must decide: `(verdict, classes, reportable mechanisms)`.
    fn expected(name: &str) -> (Verdict, &'static [&'static str], &'static [&'static str]) {
        const BOTH: &[&str] = &["concealed-rendering", "model-directive"];
        const DIRECTIVE: &[&str] = &["model-directive"];
        const CONCEALED: &[&str] = &["concealed-rendering"];
        const NONE: &[&str] = &[];
        match name {
            // `21` is `02` one operator over: mode 1 strokes and does not fill,
            // so the *stroking* colour is the only ink on the page.
            // `23` is `02` written through a `/Resources /ColorSpace` alias for
            // a device space — indirection, not a different mechanism, and the
            // alias has to be resolved or the check is suppressed.
            "02-white-on-white" | "21-stroke-only-background" | "23-aliased-device-space" => {
                (Verdict::Block, BOTH, &[mechanism::BACKGROUND_COLOUR])
            }
            // `19` is off-page by **text rise** rather than by `Td`: the
            // positioning operators read as ordinary and `Ts` does the moving.
            "03-outside-mediabox" | "19-text-rise-off-page" => {
                (Verdict::Block, BOTH, &[mechanism::OUTSIDE_MEDIA_BOX])
            }
            // `12` is `03b` with `MediaBox`/`CropBox` declared on `/Pages` and
            // inherited by the page — the only fixture that fails if the reader
            // stops at the page dictionary, which an injection run established.
            "03b-outside-cropbox" | "12-inherited-boxes" => {
                (Verdict::Block, BOTH, &[mechanism::OUTSIDE_CROP_BOX])
            }
            // `13` is `04` hidden behind a matrix: `/F1 20 Tf` inside a 1/500
            // scale. A reader that trusted the `Tf` operand sees a heading.
            // `17` is `04` restored by a `Q`; `20` collapses the glyph across
            // with `0 Tz` while `Tf` still reads 12 pt. Three spellings of "the
            // glyph occupies no area", and all three fall out of composing the
            // §9.4.4 rendering matrix rather than out of three separate tests.
            "04-degenerate-size"
            | "13-ctm-shrunk-text"
            | "17-degenerate-size-across-q"
            | "20-zero-horizontal-scale" => (Verdict::Block, BOTH, &[mechanism::DEGENERATE_SIZE]),
            // `16` is `05` with the mode set outside the text object and restored
            // by a `Q`: it is the fixture that fails unless the text state is
            // carried in the *graphics* state, per PDF 32000-1 §9.3.
            "05-render-mode-3" | "16-render-mode-across-q" => {
                (Verdict::Block, BOTH, &[mechanism::INVISIBLE_RENDER_MODE])
            }
            // The lying layer reaches `Block` through what it *says*, not through
            // the mechanism: mode 3 over a page image is how searchable scans are
            // built, so its existence is deliberately not a finding. Compare
            // `10-searchable-scan`, which is the same mechanism saying something
            // innocuous and passes.
            // `18` is `06` with the mode restored by a `Q`. It is the costliest
            // bypass of the set: read as mode 0 the run trips no check at all,
            // so its text is never collected — and a lying layer reaches
            // `block` only through what it says.
            "06-lying-layer" | "18-lying-layer-across-q" => (Verdict::Block, DIRECTIVE, &[]),
            // The declared non-goal. An ordinary text layer under an opaque image
            // is invisible to a content-stream read, so this tops out where every
            // PDF used to: `Quarantine`, on the strength of the visible directive
            // the flat extraction leaks. The body is still withheld.
            "07-under-image" => (Verdict::Quarantine, DIRECTIVE, &[]),
            // Concealment with **no** direction: the half the ladder deliberately
            // does not block on. Before this module the sentence was invisible on
            // the page, invisible to the screen, and admitted verbatim into the
            // model-facing `meta.content`. Now the document quarantines, and —
            // unlike every other fixture here — it keeps a body, with the
            // concealed sentence removed from it.
            "11-concealed-note" => (
                Verdict::Quarantine,
                CONCEALED,
                &[mechanism::BACKGROUND_COLOUR],
            ),
            _ => (Verdict::Pass, NONE, &[]),
        }
    }

    /// Screen a fixture exactly as `extract::decoded_pdf_content` does.
    fn screen(bytes: &[u8]) -> (crate::screen::Screened, Vec<Concealed>) {
        let flat = pdf_extract::extract_text_from_mem(bytes).expect("fixture extracts");
        let runs = concealed_runs(bytes);
        let regions: Vec<_> = runs.iter().map(Concealed::as_region).collect();
        (
            crate::screen::screen_text_with_concealed(&flat, &regions),
            runs,
        )
    }

    /// Every row is checked and **every** mismatch is reported.
    ///
    /// Not `assert_eq!` in the loop, which stops at the first row that differs.
    /// A single injected fault usually moves several fixtures at once — the
    /// `q`/`Q` defect found in review on PR #912 moved both `15` and `16`, in
    /// opposite directions — and a test that names only the first turns a
    /// two-sided bug into a one-sided one in the reader's mind. It also makes an
    /// injection run undercount its own coverage, which is how a fixture comes
    /// to look redundant and gets deleted.
    #[test]
    fn every_fixture_reaches_its_recorded_verdict() {
        let mut wrong: Vec<String> = Vec::new();
        for (name, bytes) in CORPUS {
            let (screened, runs) = screen(bytes);
            let (verdict, classes, mechanisms) = expected(name);
            let mut reported: Vec<&str> = runs.iter().filter_map(|r| r.mechanism).collect();
            reported.sort_unstable();
            reported.dedup();
            if screened.verdict != verdict {
                wrong.push(format!(
                    "{name}: verdict {:?}, expected {verdict:?}",
                    screened.verdict
                ));
            }
            if screened.classes() != classes {
                wrong.push(format!(
                    "{name}: classes {:?}, expected {classes:?}",
                    screened.classes()
                ));
            }
            if reported != mechanisms {
                wrong.push(format!(
                    "{name}: mechanisms {reported:?}, expected {mechanisms:?}"
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} row(s) wrong:\n  {}",
            wrong.len(),
            wrong.join("\n  ")
        );
    }

    /// The payload is recoverable from every fixture that plants one.
    ///
    /// Separate from the verdict assertion because it is the half that can fail
    /// silently: a reader that detects the mechanism but decodes nothing still
    /// reports `concealed-rendering` and still quarantines, and the document
    /// would look screened while the `block` half — the entire point of #813 —
    /// never fired. Asserting the text was recovered is what makes the `Block`
    /// above a consequence of reading the payload rather than a coincidence.
    #[test]
    fn the_concealed_payload_is_recovered_not_merely_detected() {
        let mut wrong: Vec<&str> = Vec::new();
        for (name, bytes) in CORPUS {
            let runs = concealed_runs(bytes);
            let planted = matches!(
                *name,
                "02-white-on-white"
                    | "03-outside-mediabox"
                    | "03b-outside-cropbox"
                    | "04-degenerate-size"
                    | "05-render-mode-3"
                    | "06-lying-layer"
                    | "12-inherited-boxes"
                    | "13-ctm-shrunk-text"
                    | "16-render-mode-across-q"
                    | "17-degenerate-size-across-q"
                    | "18-lying-layer-across-q"
                    | "19-text-rise-off-page"
                    | "20-zero-horizontal-scale"
                    | "21-stroke-only-background"
                    | "23-aliased-device-space"
            );
            let recovered = runs
                .iter()
                .any(|r| r.text.contains("Ignore all previous instructions"));
            if recovered != planted {
                wrong.push(name);
            }
        }
        assert!(wrong.is_empty(), "payload recovery wrong on: {wrong:?}");
    }

    /// No concealment fixture's body survives the screen.
    ///
    /// `admit` is the enforcement half: a verdict that does not withhold the text
    /// is a label. The controls keep theirs, which is what says the withholding
    /// is caused by the payload and not by the fixture's shape.
    #[test]
    fn a_directive_bearing_pdf_contributes_no_body_and_a_clean_one_keeps_its_own() {
        let mut wrong: Vec<&str> = Vec::new();
        for (name, bytes) in CORPUS {
            let (screened, _) = screen(bytes);
            // The line is *direction*, not concealment. A directive cannot be
            // redacted out of a sentence the way a run can be lifted out of a
            // body — the words are the payload — so anything carrying one loses
            // its body entirely. `11-concealed-note` is the case that separates
            // the two: concealed, quarantined, and it keeps a body.
            let directive = expected(name).1.contains(&"model-directive");
            if screened.admit.is_none() != directive {
                wrong.push(name);
            }
        }
        assert!(wrong.is_empty(), "admit/withhold wrong on: {wrong:?}");
    }

    /// The concealed run is **removed** from the body a quarantined PDF keeps.
    ///
    /// The other half of the enforcement, and the one that can fail silently: a
    /// screen that records `concealed-rendering` and stores the concealed
    /// sentence anyway has made the finding a label, and the sentence still
    /// reaches the model through `search`. Asserted end-to-end against a real
    /// PDF rather than against a synthetic string, because it depends on two
    /// independent decoders — `pdf-extract`'s and this module's — spelling the
    /// run the same way.
    #[test]
    fn a_quarantined_pdf_keeps_its_prose_and_loses_the_concealed_run() {
        let bytes = CORPUS
            .iter()
            .find(|(name, _)| *name == "11-concealed-note")
            .expect("the fixture is in the corpus")
            .1;
        let (screened, _) = screen(bytes);
        let admitted = screened.admit.expect("a body survives");
        assert!(
            admitted.contains("Ordinary prose about knowledge graphs"),
            "the visible prose must survive: {admitted:?}"
        );
        assert!(
            !admitted.contains("invisible on the page"),
            "the concealed sentence must not: {admitted:?}"
        );
    }

    /// The reader answers the same thing twice, from the same bytes.
    ///
    /// Determinism is not a nice property here, it is the eligibility condition:
    /// ADR-0019 §5 admits a `derived` fact only if it is a pure function of the
    /// input, and a verdict that is not may not be cached, may not be a graph
    /// fact, and therefore may not reach `Block` at all. The ordering half
    /// matters as much as the values — page iteration is a `BTreeMap`, and
    /// findings are pushed in content-stream order.
    #[test]
    fn the_reader_is_a_pure_function_of_the_bytes() {
        for (name, bytes) in CORPUS {
            assert_eq!(
                concealed_runs(bytes),
                concealed_runs(bytes),
                "{name}: two reads disagree"
            );
        }
    }

    /// Bytes that are not a PDF, and a PDF truncated mid-object, yield nothing
    /// and do not take the sync down with them.
    #[test]
    fn unparseable_bytes_yield_no_findings_and_do_not_panic() {
        assert!(concealed_runs(b"").is_empty());
        assert!(concealed_runs(b"not a pdf at all").is_empty());
        let (_, whole) = CORPUS[1];
        for cut in [16, whole.len() / 3, whole.len() / 2, whole.len() - 8] {
            // Only that it returns. A truncated document may still hold a
            // readable page, so the result is not asserted empty — asserting it
            // were would make the test a claim about `lopdf`'s recovery rather
            // than about this module's containment.
            let _ = concealed_runs(&whole[..cut]);
        }
    }

    /// Every mechanism this module can report is one the corpus actually
    /// exercises.
    ///
    /// A token nothing reaches is a claim of coverage with no measurement behind
    /// it, which is the exact shape #813 was filed about. Adding a mechanism
    /// without a fixture that trips it fails here.
    #[test]
    fn every_declared_mechanism_is_exercised_by_a_fixture() {
        let mut seen: Vec<&str> = CORPUS
            .iter()
            .flat_map(|(_, bytes)| concealed_runs(bytes))
            .filter_map(|r| r.mechanism)
            .collect();
        seen.sort_unstable();
        seen.dedup();
        let mut declared = mechanism::ALL.to_vec();
        declared.sort_unstable();
        assert_eq!(seen, declared);
    }

    /// `cm` composes onto the CTM in PDF's order, and a text matrix composes onto
    /// that.
    ///
    /// The unit test exists because getting this backwards is silent: the fixture
    /// corpus uses an identity CTM throughout, so a reversed multiplication
    /// passes every one of the tests above while misplacing every glyph in a real
    /// document that uses `cm` — which is most of them.
    #[expect(
        clippy::float_cmp,
        reason = "every value here is a power of two scaled by an exact integer, \
                  so the products are exact in IEEE-754 and equality is the \
                  property under test — a tolerance would pass a reversed \
                  composition that happened to land close"
    )]
    #[test]
    fn matrices_compose_in_pdf_order() {
        let scale = Matrix {
            a: 2.0,
            b: 0.0,
            c: 0.0,
            d: 2.0,
            e: 0.0,
            f: 0.0,
        };
        let shift = Matrix::translation(10.0, 20.0);
        // Translate then scale: the translation is scaled too.
        assert_eq!(shift.then(scale).apply(0.0, 0.0), (20.0, 40.0));
        // Scale then translate: it is not.
        assert_eq!(scale.then(shift).apply(0.0, 0.0), (10.0, 20.0));
        assert_eq!(scale.vertical_scale(), 2.0);
        assert_eq!(Matrix::IDENTITY.vertical_scale(), 1.0);
    }

    /// A `MediaBox` given by its opposite corners in the other order is the same
    /// box. PDF 32000-1 §7.9.5 permits either, and a reader that subtracted
    /// without normalising would place the whole page outside itself.
    #[test]
    fn rectangles_normalise_their_corners() {
        let forward = Rect::normalised(0.0, 0.0, 612.0, 792.0);
        assert_eq!(Rect::normalised(612.0, 792.0, 0.0, 0.0), forward);
        assert!(forward.contains(1.0, 1.0));
        assert!(!forward.contains(-1.0, 1.0));
        assert!(!forward.contains(1.0, 800.0));
    }

    /// Colour spaces reduce to one comparable triple, and an unreadable one
    /// reduces to `None` rather than to a guess.
    #[test]
    fn colour_operands_reduce_to_rgb_or_to_nothing() {
        use pdf_extract::Object::{Integer, Name, Real};
        assert_eq!(gray(&[Integer(1)]), Some([1.0, 1.0, 1.0]));
        assert_eq!(rgb(&[Integer(1), Integer(1), Integer(1)]), Some([1.0; 3]));
        assert_eq!(
            cmyk(&[Real(0.0), Real(0.0), Real(0.0), Real(0.0)]),
            Some([1.0; 3])
        );
        // A pattern name has no components: the fill colour becomes unknown, and
        // `inspect_run` skips the colour test rather than comparing against a
        // default that would read as black.
        assert_eq!(
            components(ColourSpace::Device, &[Name(b"P1".to_vec())]),
            None
        );
        // The same numbers are readable in a device space and **not** in any
        // other. `1 scn` is white through `/DeviceGray` and full spot ink
        // through a `/Separation`, and no arity can tell those apart — so the
        // space decides, and an unreadable one yields nothing rather than a
        // guess that would report visible text as white-on-white.
        assert_eq!(
            components(ColourSpace::Device, &[Integer(1)]),
            Some([1.0, 1.0, 1.0])
        );
        assert_eq!(components(ColourSpace::Other, &[Integer(1)]), None);
        assert!(indistinguishable([1.0; 3], [1.0 - 1.0 / 255.0; 3]));
        assert!(!indistinguishable([1.0; 3], [0.99, 1.0, 1.0]));
    }

    /// `Tr` reads its operand as an integer, and a fractional one leaves the
    /// mode alone.
    ///
    /// The specification defines the operand as an integer, so `3.7 Tr` is
    /// malformed — and a reader that truncated it would decide the document
    /// paints nothing there and report a concealment that a renderer, which also
    /// has to decide what a malformed operand means, need not agree with. The
    /// cheap, wrong version of this line (`as i64`) passes the entire fixture
    /// corpus, which is why it is asserted here rather than left to a comment.
    #[test]
    fn a_fractional_render_mode_operand_does_not_become_mode_three() {
        use pdf_extract::Object::{Integer, Real};
        let mut params = TextParameters::default();
        params.apply("Tr", &[Real(3.7)]);
        assert_eq!(params.render_mode, 0, "a malformed operand changes nothing");
        params.apply("Tr", &[Integer(3)]);
        assert_eq!(params.render_mode, 3);
    }

    /// The text state is part of the **graphics** state, so `q`/`Q` carry it and
    /// `BT` does not reset it (PDF 32000-1 §9.3 and §9.4.1).
    ///
    /// Caught in review on PR #912. The two directions are both wrong and both
    /// live: a mode set inside a `q` that leaks past the `Q` reports visible
    /// prose as concealed, and a mode reset at `BT` misses a concealment the
    /// page really carries. Fixtures `15` and `16` are those two cases
    /// end-to-end; this is the unit-level statement of the rule.
    #[test]
    fn the_text_state_is_saved_by_q_and_survives_bt() {
        use pdf_extract::Object::Integer;
        let mut gs = GraphicsState::default();
        gs.text.apply("Tr", &[Integer(3)]);

        let saved = gs.clone();
        gs.text.apply("Tr", &[Integer(0)]);
        assert_eq!(gs.text.render_mode, 0);

        // `Q`.
        gs = saved;
        assert_eq!(
            gs.text.render_mode, 3,
            "`Q` must restore the rendering mode"
        );

        // `BT` resets only the matrices — the mode is untouched by construction,
        // because it does not live in `TextState` at all.
        let text = TextState::default();
        assert_eq!(text.tm, Matrix::IDENTITY);
        assert_eq!(
            gs.text.render_mode, 3,
            "`BT` must not reset the rendering mode"
        );
    }

    /// `Td` moves the text *line* matrix, so successive lines stack rather than
    /// all landing where the first one started.
    #[test]
    fn successive_text_lines_advance_from_the_line_matrix() {
        use pdf_extract::Object::Integer;
        let mut text = TextState::default();
        let mut leading = 0.0;
        text.apply("Td", &[Integer(72), Integer(700)], &mut leading);
        assert_eq!(text.tm.apply(0.0, 0.0), (72.0, 700.0));
        text.apply("Td", &[Integer(0), Integer(-14)], &mut leading);
        assert_eq!(text.tm.apply(0.0, 0.0), (72.0, 686.0));
        leading = 14.0;
        text.apply("T*", &[], &mut leading);
        assert_eq!(text.tm.apply(0.0, 0.0), (72.0, 672.0));
    }

    /// Which colour is ink depends on the rendering mode, and every mode is
    /// checked rather than the two that happened to be on my mind.
    ///
    /// The stroke-only modes are the point: they put **no fill** on the page, so
    /// a reader that asked about the fill there was asking about ink that was
    /// never laid down — and `1 Tr` with a background-coloured stroke went
    /// undetected until the #912 review. Modes 2 and 6 are the other direction:
    /// a white fill with a black stroke is visible, and must need *both* to
    /// match before it counts.
    #[test]
    fn the_ink_of_a_run_is_whichever_colour_its_render_mode_actually_paints() {
        let white = [1.0, 1.0, 1.0];
        let black = [0.0, 0.0, 0.0];
        let state = |mode: i64| {
            let mut gs = GraphicsState::default();
            gs.text.render_mode = mode;
            gs.fill = Some(white);
            gs.stroke = Some(black);
            gs
        };
        for mode in [0, 4] {
            assert_eq!(ink_colours(&state(mode)), Some(vec![white]), "mode {mode}");
        }
        for mode in [1, 5] {
            assert_eq!(ink_colours(&state(mode)), Some(vec![black]), "mode {mode}");
        }
        for mode in [2, 6] {
            assert_eq!(
                ink_colours(&state(mode)),
                Some(vec![white, black]),
                "mode {mode} must need both"
            );
        }
        // 3 and 7 paint nothing; `inspect_run` decides those before asking.
        for mode in [3, 7] {
            assert_eq!(ink_colours(&state(mode)), None, "mode {mode}");
        }
        // An unreadable colour suppresses the check rather than standing in for
        // one — and for a fill-and-stroke mode, *either* being unknown is enough.
        let mut unknown = state(2);
        unknown.stroke = None;
        assert_eq!(ink_colours(&unknown), None);
    }

    /// `Tz` and `Ts` are part of the transform that puts a glyph on the page,
    /// and both are concealment mechanisms on their own.
    ///
    /// Asserted at the matrix rather than only through a fixture because the
    /// failure is silent in opposite directions: a reader that drops `Ts` reads
    /// an off-page glyph as on-page, and one that drops `Th` reads a collapsed
    /// glyph as 12 pt.
    #[test]
    fn the_rendering_matrix_carries_the_rise_and_the_horizontal_scale() {
        let mut params = TextParameters::default();
        params.size = 12.0;

        let plain = params.rendering_matrix();
        assert_eq!(plain.apply(0.0, 0.0), (0.0, 0.0));
        assert_eq!(plain.vertical_scale(), 12.0);
        assert_eq!(plain.horizontal_scale(), 12.0);

        // A rise moves the glyph off the origin `Td` chose.
        params.rise = 500.0;
        assert_eq!(params.rendering_matrix().apply(0.0, 0.0), (0.0, 500.0));

        // `0 Tz` collapses the glyph across while leaving it 12 pt tall.
        params.rise = 0.0;
        params.horizontal_scale = 0.0;
        let collapsed = params.rendering_matrix();
        assert_eq!(collapsed.vertical_scale(), 12.0);
        assert_eq!(collapsed.horizontal_scale(), 0.0);
    }
}
