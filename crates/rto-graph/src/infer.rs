//! The inference layer: `inferred` edges suggested from text similarity.
//!
//! This is the **lean, offline default** embedding described by ADR-0003: a
//! dependency-free hashing embedding (the "hashing trick") compiled into the
//! binary, computed on the fly with no model file and no network. It embeds each
//! node's text (name + path context) into a fixed-dimension unit vector, then
//! emits an [`EdgeKind::Related`] edge — tagged [`Provenance::Inferred`] with a
//! confidence equal to the cosine similarity — between nodes that are similar
//! but not already connected by a derived/authored fact.
//!
//! It is deliberately a *suggestion* layer (precise-where-known, fuzzy-where-
//! suggested): every edge it produces is labelled inferred and carries a
//! confidence score, and running it never changes derived or authored edges.
//! Higher-quality learned embeddings (GGUF local models) are the opt-in
//! `inference-local-models` tier from ADR-0003; this module is the fallback that
//! keeps inference working entirely offline.
//!
//! Only built with `--features inference`.

use std::collections::HashSet;

use crate::store::{Store, StoreError};
use crate::{Edge, EdgeKind, Node};

/// Dimensionality of the hashing embedding. Small enough to be cheap, large
/// enough that hash collisions between distinct tokens stay rare.
const DIM: usize = 256;

/// Provenance ref recorded on every inferred edge, identifying the embedding
/// that produced it (so a future model swap is distinguishable in the data).
const EMBED_REF: &str = "embedding:hash/v1";

/// Tuning for [`infer_edges`].
#[derive(Debug, Clone, Copy)]
pub struct InferenceConfig {
    /// Minimum cosine similarity for an edge to be emitted (`0.0..=1.0`).
    pub min_confidence: f64,
    /// Maximum inferred edges emitted per source node.
    pub top_k: usize,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            // Tuned for the hashing embedding on short identifiers: related-but-
            // distinct names (e.g. `edges_from` / `edges_by_provenance`) land
            // around 0.4–0.6, near-duplicates higher, unrelated text near 0.
            min_confidence: 0.4,
            top_k: 5,
        }
    }
}

/// A fixed-dimension unit embedding vector.
type Embedding = [f32; DIM];

/// FNV-1a (64-bit) of `s`. Dependency-free and deterministic; used only to
/// bucket tokens, so it needs no cryptographic properties.
fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Tokenise `text` into lowercase alphanumeric words plus their character
/// trigrams, so near-miss names (`edges_from` / `edges_to`) still share
/// features.
fn tokens(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out = Vec::new();
    for word in lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        out.push(word.to_owned());
        let chars: Vec<char> = word.chars().collect();
        if chars.len() >= 3 {
            for w in chars.windows(3) {
                out.push(w.iter().collect());
            }
        }
    }
    out
}

/// Embed `text` into a unit vector via signed feature hashing. Deterministic and
/// fully offline. An all-empty text yields the zero vector.
#[must_use]
pub fn embed(text: &str) -> Embedding {
    let mut v = [0f32; DIM];
    for tok in tokens(text) {
        let h = fnv1a(&tok);
        // `h % DIM` is always < DIM (256), so this conversion never fails.
        let idx = usize::try_from(h % DIM as u64).unwrap_or(0);
        // Signed hashing: the top bit picks the sign, reducing collision bias.
        let sign = if (h >> 63) & 1 == 1 { -1.0 } else { 1.0 };
        v[idx] += sign;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// Cosine similarity of two unit vectors (their dot product), clamped to
/// `0.0..=1.0` so it is a valid inferred-edge confidence.
#[must_use]
pub fn similarity(a: &Embedding, b: &Embedding) -> f64 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    f64::from(dot).clamp(0.0, 1.0)
}

/// The text embedded for a node: its name plus the file stem for a little path
/// context.
fn node_text(node: &Node) -> String {
    let mut text = node.name.clone();
    if let Some(path) = &node.path {
        let stem = path.rsplit('/').next().unwrap_or(path);
        text.push(' ');
        text.push_str(stem);
    }
    text
}

/// Suggest `inferred` similarity edges over the whole graph.
///
/// Every node is embedded; for each source, the most-similar other nodes above
/// `config.min_confidence` (up to `config.top_k`) that are not already connected
/// to it become [`EdgeKind::Related`] edges with `provenance = inferred` and
/// `confidence = similarity`. Pairs already joined by any existing edge (in
/// either direction) are skipped, so inference never re-states a known fact.
/// Output is deterministic (ties broken by key).
///
/// # Errors
/// Returns [`StoreError`] if the store cannot be read.
pub fn infer_edges(store: &Store, config: InferenceConfig) -> Result<Vec<Edge>, StoreError> {
    // Embed every node once.
    let keys = store.all_keys()?;
    let mut nodes: Vec<(String, Embedding)> = Vec::with_capacity(keys.len());
    for key in &keys {
        if let Some(node) = store.get_node(key)? {
            nodes.push((node.key.clone(), embed(&node_text(&node))));
        }
    }

    // Existing directed pairs, so we never suggest what is already a fact.
    let mut existing: HashSet<(String, String)> = HashSet::new();
    for (key, _) in &nodes {
        for edge in store.edges_from(key)? {
            existing.insert((edge.src, edge.dst));
        }
    }
    let connected = |a: &str, b: &str| {
        existing.contains(&(a.to_owned(), b.to_owned()))
            || existing.contains(&(b.to_owned(), a.to_owned()))
    };

    let mut edges = Vec::new();
    for (i, (src, src_vec)) in nodes.iter().enumerate() {
        // Score against every other node.
        let mut candidates: Vec<(f64, &str)> = Vec::new();
        for (j, (dst, dst_vec)) in nodes.iter().enumerate() {
            if i == j || connected(src, dst) {
                continue;
            }
            let sim = similarity(src_vec, dst_vec);
            if sim >= config.min_confidence {
                candidates.push((sim, dst.as_str()));
            }
        }
        // Highest similarity first; ties broken by key for determinism.
        candidates.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(b.1))
        });
        for (sim, dst) in candidates.into_iter().take(config.top_k) {
            let mut edge = Edge::inferred(src.clone(), dst.to_owned(), EdgeKind::Related, sim);
            edge.src_ref = Some(EMBED_REF.to_owned());
            edges.push(edge);
        }
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::{InferenceConfig, embed, infer_edges, similarity};
    use crate::{EdgeKind, FactSet, Node, NodeKind, Provenance, Store};

    #[test]
    fn embedding_is_deterministic_and_unit_length() {
        let a = embed("Store::apply_factset");
        let b = embed("Store::apply_factset");
        // Compare bit patterns: the same deterministic computation yields
        // bit-identical floats, and this avoids a float-equality lint.
        let bits = |v: &[f32; super::DIM]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(&a), bits(&b), "embedding must be deterministic");
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "unit length, got {norm}");
    }

    #[test]
    fn similar_names_score_higher_than_unrelated() {
        let from = embed("edges_from");
        let to = embed("edges_to");
        let far = embed("cloudflare deployment pipeline");
        let near = similarity(&from, &to);
        let distant = similarity(&from, &far);
        assert!(
            near > distant,
            "near {near} should exceed distant {distant}"
        );
        assert!(near > 0.3, "related names should share features: {near}");
    }

    #[test]
    fn similarity_is_in_range() {
        let a = embed("anything at all");
        assert!((0.0..=1.0).contains(&similarity(&a, &a)));
        assert!((0.0..=1.0).contains(&similarity(&a, &embed(""))));
    }

    #[test]
    fn infers_confident_related_edges_and_skips_known_facts() {
        let mut store = Store::open_in_memory().expect("store");
        let facts = FactSet::new()
            .with_node(Node::new(
                "sym:rust:a.rs#edges_from",
                NodeKind::Fn,
                "edges_from",
            ))
            .with_node(Node::new(
                "sym:rust:a.rs#edges_to",
                NodeKind::Fn,
                "edges_to",
            ))
            .with_node(Node::new(
                "sym:rust:a.rs#edges_by_provenance",
                NodeKind::Fn,
                "edges_by_provenance",
            ))
            .with_node(Node::new("sym:rust:a.rs#unrelated", NodeKind::Fn, "quokka"))
            // A pre-existing derived edge between two of the similar fns:
            // inference must NOT re-suggest this known pair.
            .with_edge(crate::Edge::derived(
                "sym:rust:a.rs#edges_from",
                "sym:rust:a.rs#edges_to",
                EdgeKind::Calls,
            ));
        store.apply_factset(&facts).expect("apply");

        let inferred = infer_edges(&store, InferenceConfig::default()).expect("infer");

        // Every inferred edge is labelled inferred, is `related`, carries a
        // confidence in range, and is not the already-known edges_from->edges_to.
        assert!(
            !inferred.is_empty(),
            "should infer at least one edge among the unconnected similar fns",
        );
        for e in &inferred {
            assert_eq!(e.provenance, Provenance::Inferred);
            assert_eq!(e.kind, EdgeKind::Related);
            let c = e.confidence.expect("confidence present");
            assert!((0.0..=1.0).contains(&c));
            assert!(e.is_valid());
            assert!(
                !(e.src == "sym:rust:a.rs#edges_from" && e.dst == "sym:rust:a.rs#edges_to"),
                "must not re-suggest an existing edge",
            );
        }
        // The unrelated `quokka` node shares no features, so it is never linked.
        assert!(
            inferred
                .iter()
                .all(|e| e.dst != "sym:rust:a.rs#unrelated" && e.src != "sym:rust:a.rs#unrelated"),
            "unrelated node must not be inferred-linked",
        );

        // Applying the inferred edges is accepted by the store (invariants hold).
        let mut s2 = store;
        s2.apply_factset(&FactSet {
            nodes: vec![],
            edges: inferred,
        })
        .expect("inferred edges satisfy store invariants");
        assert!(
            !s2.edges_by_provenance(Provenance::Inferred)
                .expect("q")
                .is_empty()
        );
    }

    #[test]
    fn top_k_bounds_edges_per_source() {
        let mut store = Store::open_in_memory().expect("store");
        let mut facts = FactSet::new();
        // Ten near-identical names → many candidates per source.
        for i in 0..10 {
            facts = facts.with_node(Node::new(
                format!("sym:rust:a.rs#handler{i}"),
                NodeKind::Fn,
                format!("handler{i}"),
            ));
        }
        store.apply_factset(&facts).expect("apply");

        let cfg = InferenceConfig {
            min_confidence: 0.3,
            top_k: 2,
        };
        let inferred = infer_edges(&store, cfg).expect("infer");
        for key in store.all_keys().expect("keys") {
            let from_key = inferred.iter().filter(|e| e.src == key).count();
            assert!(
                from_key <= 2,
                "top_k=2 bound exceeded for {key}: {from_key}"
            );
        }
    }
}
