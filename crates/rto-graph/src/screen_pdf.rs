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
//! # What is deliberately **not** detected, and why
//!
//! Stated here, in [`crate::screen`]'s own non-goals list, in `docs/`, and in
//! what `roteiro sync` prints when it accepts a PDF. A screen that runs, passes,
//! and implies a check it did not perform is worse than no screen: it reads as
//! coverage. #813 and ADR-0025 both settle on the same standard — *a narrower
//! guard that states its limits beats a broad one that quietly checks nothing.*
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
//! **No opacity, no clipping, no transparency groups.** `q`/`Q` and `cm` are
//! tracked; `gs` (an `ExtGState`, which can set a constant alpha), `W`/`W*`
//! clipping paths, soft masks and blend modes are not. Text made invisible by an
//! alpha of zero or clipped away entirely is not detected.
//!
//! **No glyph advance.** The origin of each text-showing operator is tested, not
//! the box the run sweeps out. A line that starts inside the page and runs off
//! the edge is not reported.
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
            "g" | "G" => set_fill(&mut gs, op.operator.as_str(), gray(operands)),
            "rg" | "RG" => set_fill(&mut gs, op.operator.as_str(), rgb(operands)),
            "k" | "K" => set_fill(&mut gs, op.operator.as_str(), cmyk(operands)),
            // `sc`/`scn` set a colour in whatever space `cs` selected. Numeric
            // operands are read by arity; a pattern or a named colour leaves the
            // fill **unknown**, which suppresses the colour test rather than
            // guessing at it.
            "sc" | "scn" => gs.fill = components(operands),
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
            "BT" => text = TextState::default(),
            "Tf" | "Tr" | "TL" | "Tm" | "Td" | "TD" | "T*" => {
                text.apply(op.operator.as_str(), operands);
            }
            "Tj" | "TJ" | "'" | "\"" => {
                if op.operator == "'" || op.operator == "\"" {
                    text.next_line(0.0, -text.leading);
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
            // Every other operator is read past, and four are worth naming
            // because their absence looks like an oversight:
            //
            // * `ET` closes a text object whose state the next `BT` resets;
            // * `SC`/`SCN` and `G`/`RG`/`K` set the **stroking** colour, which
            //   no check here reads (see `set_fill`);
            // * `cs`/`CS` select a colour space this reader does not resolve —
            //   `sc`/`scn` are read by arity instead, and an arity it does not
            //   recognise leaves the colour unknown rather than guessed;
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
    let trm = run.text.tm.then(run.gs.ctm);
    let (x, y) = trm.apply(0.0, 0.0);
    let effective = run.text.size.abs() * trm.vertical_scale();
    let behind = topmost(run.painted, x, y);

    let mut found: Vec<(&'static str, String)> = Vec::new();
    let mut unreported: Option<String> = None;

    // Modes 3 and 7 paint nothing at all. 7 adds to the clipping path, which is
    // not a mark on the page either; listing both is the difference between
    // detecting the mechanism and detecting one spelling of it.
    if matches!(run.text.render_mode, 3 | 7) {
        if matches!(behind, Paint::Opaque) {
            unreported = Some(
                "invisible text layer over a page image (the ordinary searchable-scan shape)"
                    .to_owned(),
            );
        } else {
            found.push((
                mechanism::INVISIBLE_RENDER_MODE,
                format!("text rendering mode {}", run.text.render_mode),
            ));
        }
    } else if fills(run.text.render_mode)
        && let Some(ink) = run.gs.fill
        && let Paint::Solid(background) = behind
        && indistinguishable(ink, background)
    {
        found.push((
            mechanism::BACKGROUND_COLOUR,
            "fill colour indistinguishable from what is painted behind it".to_owned(),
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

    // `run.text.font.is_some()` is exactly "a `Tf` has executed in this text
    // object": the operator sets the font and the size together. Without it a
    // malformed document that shows text with no font selected would be reported
    // as concealing it at the default size of zero, which is a claim about the
    // parser's state rather than about the page.
    if run.text.font.is_some() && effective < MIN_LEGIBLE_SIZE {
        found.push((
            mechanism::DEGENERATE_SIZE,
            format!("effective text size {effective:.4} pt"),
        ));
    }

    if found.is_empty() && unreported.is_none() {
        return;
    }
    let decoded = decode_run(run.doc, run.fonts, run.text.font.as_deref(), strings);
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

/// Whether a text rendering mode fills glyphs, and so has a fill colour that
/// means anything. Modes 0, 2, 4 and 6 fill; 1 and 5 stroke only; 3 and 7 paint
/// nothing (PDF 32000-1 §9.3.6).
fn fills(mode: i64) -> bool {
    matches!(mode, 0 | 2 | 4 | 6)
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
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            fill: Some([0.0, 0.0, 0.0]),
        }
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
    size: f64,
    leading: f64,
    render_mode: i64,
    font: Option<Vec<u8>>,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            tm: Matrix::IDENTITY,
            tlm: Matrix::IDENTITY,
            size: 0.0,
            leading: 0.0,
            render_mode: 0,
            font: None,
        }
    }
}

impl TextState {
    /// Apply one text-state operator. Split out of the content-stream walk so
    /// that the state a text object carries is changed in exactly one place.
    fn apply(&mut self, operator: &str, operands: &[Object]) {
        let first = operands.first().and_then(number).unwrap_or(0.0);
        let second = operands.get(1).and_then(number).unwrap_or(0.0);
        match operator {
            "Tf" => {
                self.font = operands
                    .first()
                    .and_then(|o| o.as_name().ok())
                    .map(<[u8]>::to_vec);
                self.size = second;
            }
            // The operand is an integer by the specification, so this reads it as
            // one rather than rounding a float: a `Tr` operand that is not a
            // whole number is malformed, and mode 0 (fill) is the value the
            // specification itself defaults to.
            "Tr" => self.render_mode = operands.first().and_then(integer).unwrap_or(0),
            "TL" => self.leading = first,
            "Tm" => {
                if let Some(m) = matrix(operands) {
                    self.tm = m;
                    self.tlm = m;
                }
            }
            "Td" => self.next_line(first, second),
            // `TD` is `-ty TL` followed by `tx ty Td`.
            "TD" => {
                self.leading = -second;
                self.next_line(first, second);
            }
            "T*" => self.next_line(0.0, -self.leading),
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

/// Only the *non*-stroking operators set the colour glyphs are filled with; the
/// uppercase forms set the stroking colour, which no check here reads.
fn set_fill(gs: &mut GraphicsState, operator: &str, value: Option<[f64; 3]>) {
    if operator.chars().next().is_some_and(char::is_lowercase) {
        gs.fill = value;
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

/// `sc`/`scn` operands, read by arity because the colour space was set by a
/// separate `cs` this reader does not resolve. A pattern name or any other
/// arity leaves the colour unknown.
fn components(operands: &[Object]) -> Option<[f64; 3]> {
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
        "08-scaled-text-matrix",
        "09-white-text-over-image",
        "10-searchable-scan",
        "14-scaled-placement",
    ];

    /// What each fixture must decide: `(verdict, classes, reportable mechanisms)`.
    fn expected(name: &str) -> (Verdict, &'static [&'static str], &'static [&'static str]) {
        const BOTH: &[&str] = &["concealed-rendering", "model-directive"];
        const DIRECTIVE: &[&str] = &["model-directive"];
        const CONCEALED: &[&str] = &["concealed-rendering"];
        const NONE: &[&str] = &[];
        match name {
            "02-white-on-white" => (Verdict::Block, BOTH, &[mechanism::BACKGROUND_COLOUR]),
            "03-outside-mediabox" => (Verdict::Block, BOTH, &[mechanism::OUTSIDE_MEDIA_BOX]),
            // `12` is `03b` with `MediaBox`/`CropBox` declared on `/Pages` and
            // inherited by the page — the only fixture that fails if the reader
            // stops at the page dictionary, which an injection run established.
            "03b-outside-cropbox" | "12-inherited-boxes" => {
                (Verdict::Block, BOTH, &[mechanism::OUTSIDE_CROP_BOX])
            }
            // `13` is `04` hidden behind a matrix: `/F1 20 Tf` inside a 1/500
            // scale. A reader that trusted the `Tf` operand sees a heading.
            "04-degenerate-size" | "13-ctm-shrunk-text" => {
                (Verdict::Block, BOTH, &[mechanism::DEGENERATE_SIZE])
            }
            "05-render-mode-3" => (Verdict::Block, BOTH, &[mechanism::INVISIBLE_RENDER_MODE]),
            // The lying layer reaches `Block` through what it *says*, not through
            // the mechanism: mode 3 over a page image is how searchable scans are
            // built, so its existence is deliberately not a finding. Compare
            // `10-searchable-scan`, which is the same mechanism saying something
            // innocuous and passes.
            "06-lying-layer" => (Verdict::Block, DIRECTIVE, &[]),
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

    #[test]
    fn every_fixture_reaches_its_recorded_verdict() {
        for (name, bytes) in CORPUS {
            let (screened, runs) = screen(bytes);
            let (verdict, classes, mechanisms) = expected(name);
            assert_eq!(screened.verdict, verdict, "{name}: verdict");
            assert_eq!(screened.classes(), classes, "{name}: classes");
            let mut reported: Vec<&str> = runs.iter().filter_map(|r| r.mechanism).collect();
            reported.sort_unstable();
            reported.dedup();
            assert_eq!(reported, mechanisms, "{name}: reported mechanisms");
        }
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
            );
            let recovered = runs
                .iter()
                .any(|r| r.text.contains("Ignore all previous instructions"));
            assert_eq!(recovered, planted, "{name}: payload recovery");
        }
    }

    /// No concealment fixture's body survives the screen.
    ///
    /// `admit` is the enforcement half: a verdict that does not withhold the text
    /// is a label. The controls keep theirs, which is what says the withholding
    /// is caused by the payload and not by the fixture's shape.
    #[test]
    fn a_directive_bearing_pdf_contributes_no_body_and_a_clean_one_keeps_its_own() {
        for (name, bytes) in CORPUS {
            let (screened, _) = screen(bytes);
            // The line is *direction*, not concealment. A directive cannot be
            // redacted out of a sentence the way a run can be lifted out of a
            // body — the words are the payload — so anything carrying one loses
            // its body entirely. `11-concealed-note` is the case that separates
            // the two: concealed, quarantined, and it keeps a body.
            let directive = expected(name).1.contains(&"model-directive");
            assert_eq!(
                screened.admit.is_none(),
                directive,
                "{name}: admit withheld"
            );
        }
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
        assert_eq!(components(&[Name(b"P1".to_vec())]), None);
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
        let mut text = TextState::default();
        text.apply("Tr", &[Real(3.7)]);
        assert_eq!(text.render_mode, 0, "a malformed operand changes nothing");
        text.apply("Tr", &[Integer(3)]);
        assert_eq!(text.render_mode, 3);
    }

    /// `Td` moves the text *line* matrix, so successive lines stack rather than
    /// all landing where the first one started.
    #[test]
    fn successive_text_lines_advance_from_the_line_matrix() {
        use pdf_extract::Object::Integer;
        let mut text = TextState::default();
        text.apply("Td", &[Integer(72), Integer(700)]);
        assert_eq!(text.tm.apply(0.0, 0.0), (72.0, 700.0));
        text.apply("Td", &[Integer(0), Integer(-14)]);
        assert_eq!(text.tm.apply(0.0, 0.0), (72.0, 686.0));
        text.apply("TL", &[Integer(14)]);
        text.apply("T*", &[]);
        assert_eq!(text.tm.apply(0.0, 0.0), (72.0, 672.0));
    }

    /// Only the modes that put a filled glyph on the page have a fill colour
    /// worth comparing. Getting this wrong reports every stroke-only heading as
    /// hidden in its background.
    #[test]
    fn only_filling_render_modes_carry_a_fill_colour() {
        for mode in [0, 2, 4, 6] {
            assert!(fills(mode), "mode {mode} fills");
        }
        for mode in [1, 3, 5, 7] {
            assert!(!fills(mode), "mode {mode} does not fill");
        }
    }
}
