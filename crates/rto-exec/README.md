# rto-exec

The analyzer execution seam for [Roteiro](https://roteiro.dev).

External analyzers (`cargo-audit`, `semgrep`, and successors) can be run in more
than one place: in CI, on a developer's own machine, or — later — locally inside
a sandbox. This crate exists so those are **not competing architectures**. One
trait, `AnalyzerRunner`, defines a single request and a single normalized
response; every backend satisfies it, so callers never learn which one produced a
result.

Today there is exactly one implementation, `IngestRunner`, which consumes a
normalized JSON report produced anywhere and yields the same `Finding` and
`AnalysisRun` values any other backend would. It requires no install, no
container runtime, and no isolation surface. A subprocess backend and a
sandboxed (microVM) backend are planned behind their own features and will not
change this crate's callers.

Results are persisted by `rto-graph` as a **separate artifact store**: findings
are never nodes or edges, never acquire a provenance class, and never appear in
the exported graph artifact. See ADR-0012 (the findings artifact model) and
ADR-0014 (sandboxed analyzer execution).

Licensed under MIT OR Apache-2.0.
