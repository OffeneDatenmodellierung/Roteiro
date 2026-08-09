# Provenance

Every edge in the graph records **how it was produced** — `derived` (a pure
function of the AST), `authored` (human/agent intent), or `inferred` (a
heuristic, carrying a confidence). A query can therefore separate what the
compiler knows from what a person decided. The store enforces this on write;
see [[architecture#Graph store]].

## Authored layer

ADRs and this lat.md graph form the authored layer: `[[wiki-links]]` into code
become `authored` edges over the derived graph. They are validated — a link to a
symbol that no longer exists is pruned rather than kept as stale data — so the
authored layer stays honest as the code changes.
