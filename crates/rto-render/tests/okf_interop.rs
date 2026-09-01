//! Read OKF bundles **somebody else wrote**.
//!
//! Phase 1 (#708) landed the OKF reader and exercised it only against Roteiro's
//! own `render okf` output. That proves a round trip, which is the floor rather
//! than the goal: ADR-0021 adopted OKF because it is *vendor-neutral*, and a
//! reader tested solely against its own writer will happily agree with itself
//! about a format it also invents. "We read OKF" was, until this file existed, a
//! claim about a dialect we also spoke.
//!
//! The fixtures are two of the four bundles published in the specification's own
//! repository — see `tests/fixtures/okf-upstream/PROVENANCE.md` for their
//! commit, licence and the trimming applied. They are the closest thing there is
//! to an authoritative answer about what a conformant v0.2 bundle looks like,
//! because the people who wrote the specification wrote them.
//!
//! # What these caught
//!
//! All of it, on the first run. The reader hand-parsed a line-oriented YAML
//! subset shaped like its own writer's output, and against real third-party
//! markdown it silently:
//!
//! - dropped every **flow mapping** (`generated: { by: …, at: … }`), so all nine
//!   `acme_retail` concepts read as *unverified* when eight carry a human
//!   sign-off — making `import --from okf --trust` adopt nothing while reporting
//!   success;
//! - dropped **flow sequences** (`tags: [finance, revenue]`);
//! - dropped **block sequences at the parent key's indentation**, `PyYAML`'s
//!   default, taking `ga4`'s `tags` and `sources` with them;
//! - **truncated** a folded multi-line `description` at its first line.
//!
//! Every one was silent: nothing skipped, nothing reported. The counts below are
//! cross-checked against an independent pure-Rust v0.2 implementation
//! (`okf-validator`, <https://github.com/W4G1/okf>), which reads the same
//! bundles as 8 human-reviewed and 1 unverified — exactly what
//! [`a_google_bundle_keeps_its_human_verifiers`] asserts.

use std::path::{Path, PathBuf};

use rto_graph::Provenance;
use rto_render::okf::read::{ReadOptions, Trust, read_bundle};

/// The vendored upstream bundles.
fn fixture(bundle: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/okf-upstream")
        .join(bundle)
}

/// Walk a bundle into the `(bundle-relative path, content)` pairs the reader
/// takes, the same way the CLI's `read_bundle_files` does: `.md` only, sorted,
/// each path leading-slashed.
fn walk(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).expect("read fixture dir");
        for entry in entries {
            let entry = entry.expect("fixture dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                let rel = path
                    .strip_prefix(root)
                    .expect("fixture path under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                let content = std::fs::read_to_string(&path).expect("read fixture file");
                out.push((format!("/{rel}"), content));
            }
        }
    }
    out.sort();
    out
}

/// Read a fixture bundle at the given trust mode.
fn read(bundle: &str, trust: Trust) -> rto_render::okf::read::OkfImport {
    let root = fixture(bundle);
    let files = walk(&root);
    assert!(
        !files.is_empty(),
        "fixture `{bundle}` has no markdown files"
    );
    let opts = ReadOptions {
        trust,
        peer: bundle,
        extref_keys: &[],
    };
    read_bundle(&root.to_string_lossy(), &files, &opts).expect("read upstream bundle")
}

/// Find the imported node for one bundle-relative concept path.
fn concept<'a>(
    import: &'a rto_render::okf::read::OkfImport,
    bundle: &str,
    path: &str,
) -> &'a rto_graph::Node {
    let key = format!("okf:{bundle}{path}");
    import
        .facts
        .nodes
        .iter()
        .find(|n| n.key == key)
        .unwrap_or_else(|| panic!("no imported node keyed `{key}`"))
}

/// The specification's own `acme_retail` bundle writes `generated` and
/// `verified` as **flow mappings**, which is the form `SPEC.md`'s examples use
/// throughout. Eight of its nine concepts carry a `human:` verifier, and under
/// `Trust::Trust` §5.3's human-reviewed tier must survive the import — that is
/// the entire promise of the `--trust` flag.
///
/// Asserted as whole values, not counts alone: a tier that is right in aggregate
/// and wrong per document would still pass a bare total.
#[test]
fn a_google_bundle_keeps_its_human_verifiers() {
    let import = read("acme_retail", Trust::Trust);

    assert_eq!(
        import.report.concepts_read, 9,
        "acme_retail holds nine concept documents"
    );
    assert_eq!(
        import.report.skipped,
        Vec::new(),
        "no document in a published, conformant bundle should be skipped"
    );

    let tiers: Vec<(String, usize)> = import
        .report
        .concepts_by_provenance
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    assert_eq!(
        tiers,
        vec![
            ("external-authored".to_owned(), 8),
            ("external-inferred".to_owned(), 1),
        ],
        "eight of nine acme_retail concepts carry a `human:` verifier; the ninth \
         (`skills/run-on-bq.md`) carries no `verified` key at all. An independent \
         OKF v0.2 implementation reads the same split as 8 human-reviewed and 1 \
         unverified"
    );

    // The whole record for one concept, so a tier that is right for the wrong
    // reason cannot pass. `sources`, `tags` and `description` are here because
    // each was silently lost by the line-oriented parser this replaced.
    let node = concept(&import, "acme_retail", "/computations/revenue-ytd.md");
    assert_eq!(node.provenance, Provenance::ExternalAuthored);
    let okf = &node.meta["okf"];
    assert_eq!(
        okf["origin"],
        serde_json::json!({
            "by": "human:jsmith@acme",
            "at": "2026-07-01T09:00:00Z",
            "confirms": true,
        }),
        "the peer's own verifier, verbatim — `render okf` re-emits this by name"
    );
    assert_eq!(
        okf["tags"],
        serde_json::json!(["finance", "revenue", "attested"]),
        "written as a flow sequence `tags: [finance, revenue, attested]`"
    );
    assert_eq!(
        okf["sources"],
        serde_json::json!(["policies/revenue-recognition.md", "tables/orders.md",]),
        "both `sources[].resource` values, in document order"
    );
    assert_eq!(
        okf["claimed"],
        serde_json::json!({ "tier": "authored", "verified": true }),
        "what the bundle claimed, kept as data alongside what we accepted"
    );
}

/// The same bundle under `Trust::Acknowledge` must import the same nine concepts
/// and adopt **none** of their confirmations, while still recording what it
/// declined. Without this, a green [`a_google_bundle_keeps_its_human_verifiers`]
/// would be satisfied by a reader that simply trusted everything.
#[test]
fn acknowledging_a_google_bundle_adopts_none_of_its_confirmations() {
    let import = read("acme_retail", Trust::Acknowledge);

    let tiers: Vec<(String, usize)> = import
        .report
        .concepts_by_provenance
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    assert_eq!(
        tiers,
        vec![("external-inferred".to_owned(), 9)],
        "acknowledging a peer imports their concepts without re-asserting a \
         single one of their confirmations"
    );

    let node = concept(&import, "acme_retail", "/computations/revenue-ytd.md");
    assert_eq!(node.provenance, Provenance::ExternalInferred);
    assert_eq!(
        node.meta["okf"]["claimed"],
        serde_json::json!({ "tier": "authored", "verified": true }),
        "the claim is still recorded, so the same import can be re-run as trust \
         without re-reading the bundle"
    );
    assert_eq!(
        node.meta["okf"]["trust"],
        serde_json::json!("acknowledge"),
        "and the mode that was used is on the record"
    );
}

/// `ga4` is machine-serialised in `PyYAML`'s default style: block sequence items
/// sit at the parent key's own indentation, and long scalars are folded across
/// lines. Both are ordinary YAML and neither survived the line-oriented parser —
/// the folded `description` was *truncated* mid-sentence rather than dropped,
/// which is the worse failure of the two because the result still looks like a
/// description.
#[test]
fn a_machine_serialised_bundle_keeps_its_tags_and_whole_description() {
    let import = read("ga4", Trust::Trust);
    let node = concept(&import, "ga4", "/tables/events_.md");
    let okf = &node.meta["okf"];

    assert_eq!(
        okf["description"],
        serde_json::json!(
            "Google Analytics 4 event-level daily sharded export tables containing user \
             interaction logs."
        ),
        "the folded scalar's continuation line belongs to the description; \
         truncating at `containing` yields a sentence that reads as complete and \
         is not"
    );
    assert_eq!(
        okf["tags"],
        serde_json::json!(["analytics", "e-commerce", "ga4", "sharded-tables"]),
        "a block sequence whose items are not indented past the key is still a \
         sequence"
    );
    assert_eq!(
        okf["sources"],
        serde_json::json!([
            "https://support.google.com/analytics/answer/7029846",
            "https://bigquery.googleapis.com/v2/projects/bigquery-public-data/datasets/ga4_obfuscated_sample_ecommerce/tables/events_*",
            "https://support.google.com/analytics/answer/9037342",
        ]),
        "and so is `sources`, written the same way"
    );
    assert_eq!(
        okf["origin"],
        serde_json::json!({
            "by": "reference_agent/gemini-3.5-flash",
            "at": "2026-07-10T21:15:20+00:00",
            "confirms": false,
        }),
        "a `<producer>/<version>` actor (§7) and an offset-bearing timestamp, \
         neither of which is the form Roteiro's own writer emits"
    );
}

/// §5.2: *"a single verifier MAY be written as one `{ by, at }` mapping without
/// the list dash. Consumers MUST treat a bare mapping as a one-element list."*
///
/// That is one of only three MUSTs §11 places on a consumer, and no upstream
/// fixture exercises it — every published bundle writes `verified` as a list. So
/// it is asserted directly, against the shape the specification prints, rather
/// than left to a bundle that happens not to use it.
#[test]
fn a_bare_verified_mapping_is_read_as_a_one_element_list() {
    let bare = "---\n\
                type: Metric\n\
                title: Revenue\n\
                verified: { by: human:ahormati, at: 2026-06-25T09:00:00Z }\n\
                ---\n\n\
                # Definition\n";
    let listed = "---\n\
                  type: Metric\n\
                  title: Revenue\n\
                  verified:\n\
                  \x20 - { by: human:ahormati, at: 2026-06-25T09:00:00Z }\n\
                  ---\n\n\
                  # Definition\n";

    let read_one = |content: &str| {
        let files = vec![("/metrics/revenue.md".to_owned(), content.to_owned())];
        let opts = ReadOptions {
            trust: Trust::Trust,
            peer: "peer",
            extref_keys: &[],
        };
        let import = read_bundle("bundle", &files, &opts).expect("read");
        let node = import
            .facts
            .nodes
            .iter()
            .find(|n| n.key == "okf:peer/metrics/revenue.md")
            .expect("the concept")
            .clone();
        (node.provenance, node.meta["okf"]["origin"].clone())
    };

    let (bare_prov, bare_origin) = read_one(bare);
    let (listed_prov, listed_origin) = read_one(listed);

    assert_eq!(
        bare_prov,
        Provenance::ExternalAuthored,
        "a bare `verified` mapping names a `human:` actor, so §5.3 puts the \
         concept in the human-reviewed tier"
    );
    assert_eq!(
        bare_origin,
        serde_json::json!({
            "by": "human:ahormati",
            "at": "2026-06-25T09:00:00Z",
            "confirms": true,
        })
    );
    assert_eq!(
        (bare_prov, bare_origin),
        (listed_prov, listed_origin),
        "§5.2 makes the two spellings the same document; a consumer that reads \
         them differently has a bug the spec names in advance"
    );
}
