//! Prose claims in the project blueprint that a test can read the code for.
//!
//! `roteiro check` validates the blueprint's `[[links]]`, not its sentences, so a
//! stated cardinality of a closed enum resolves no link and drifts silently —
//! `docs/blueprint/roteiro.md` said "three provenance classes" for two releases
//! after the enum grew to six (#803). This pins that one class of claim; it is
//! not a prose checker.

use std::path::Path;

use rto_graph::Provenance;

fn blueprint() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/blueprint/roteiro.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The blueprint's provenance count, and each token it names, match the enum.
#[test]
fn the_blueprint_states_the_provenance_cardinality_the_enum_has() {
    let text = blueprint();
    let tokens = Provenance::tokens();
    let word = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight",
    ]
    .get(tokens.len())
    .expect("a count small enough to spell");
    assert!(
        text.contains(&format!("one of {word} provenance tokens")),
        "the blueprint must say \"one of {word} provenance tokens\" — the enum has {}",
        tokens.len()
    );
    for token in tokens {
        assert!(
            text.contains(&format!("**{token}**")) || text.contains(&format!("`{token}`")),
            "the blueprint never names the `{token}` provenance token"
        );
    }
}
