# Architecture Decision Records

> The ADRs record **decisions**. Their sibling in the authoring pillar (ADR-0004)
> is the overall **[Project Blueprint](../blueprint/roteiro.md)** — a
> graph-grounded, `check`-validated map of how the whole system fits together.

| ADR | Title | State |
|---|---|---|
| [0001](0001-build-roteiro-unified-codebase-knowledge-graph.md) | Build Roteiro — a unified, provenance-tagged codebase knowledge graph (spec-store v2) | Accepted |
| [0002](0002-adopt-rmcp-for-networked-mcp-serving.md) | Adopt the official rmcp SDK for networked MCP serving | Accepted |
| [0003](0003-pluggable-embedding-models.md) | Pluggable embedding models — tiny static default, opt-in local models | Accepted |
| [0004](0004-spec-blueprint-authoring-pillar.md) | Spec/Blueprint authoring pillar — tiered, graph-grounded, check-gated | Accepted |
| [0005](0005-image-ocr-vision-ingestion.md) | Image ingestion — tiered OCR (pure-Rust) + optional vision understanding | Accepted |
| [0006](0006-local-model-serving.md) | Local model serving — reuse pulled models over an OpenAI-compatible endpoint | Accepted |
| [0007](0007-configuration-file.md) | Configuration file — a single project-level TOML with layered precedence | Accepted |
| [0008](0008-multi-repo-workspace-serve.md) | Multi-repo workspace serve — one instance, many project graphs, one model | Accepted |
| [0009](0009-cross-repo-workspace-links.md) | Cross-repo workspace links — interlink a hub app with its deployment repos | Accepted |
| [0010](0010-explorer-web-app-vendored-js.md) | Explorer web app — vendored client-side JS (cytoscape.js) for the served UI | Accepted |
| [0011](0011-structured-file-logging-otel-groundwork.md) | Structured file logging — OpenTelemetry-shaped JSON, rotated, groundwork for OTLP | Accepted |
| [0012](0012-analyzer-findings-artifact-model.md) | Analyzer findings — a separate artifact model, never a provenance class | Accepted |
| [0013](0013-agent-memory-artifact-store.md) | Agent memory — a two-tier artifact store, decaying by evidence not by clock | Accepted |
| [0014](0014-sandboxed-analyzer-execution.md) | Sandboxed analyzer execution — an owned seam, ingest by default, boxlite opt-in | Accepted |
| [0015](0015-generated-media-content-artifact-store.md) | Generated media content — its own artifact store, rebuildable on demand | Accepted |
| [0016](0016-audio-metadata-extraction.md) | Audio metadata as derived facts — symphonia for format reads, MPL-2.0 allowed | Accepted |
| [0017](0017-dependency-security-policy.md) | Dependency security — current by default, monitored, held for a minimum release age of at least 48 hours | Accepted |
| [0018](0018-analyzer-coverage-matrix.md) | Analyzer coverage — which analyzers deliver which languages, and on which axis | Accepted |
| [0019](0019-remote-model-tier.md) | Remote model tier — an explicitly consented egress path, and the promises it changes | Accepted |
| [0020](0020-build-capable-sandboxed-execution.md) | Build-capable sandboxed execution — running the repository's own build, and the non-goal it narrows | Draft |
| [0021](0021-open-knowledge-format-bundle.md) | The graph's shareable form is an OKF bundle — replacing the Obsidian vault | Accepted |
| [0022](0022-dynamic-okf-viewer.md) | A dynamic OKF viewer — the bundle is the source, and it is somebody else's | Accepted |

> **ADR 0018** is the third of the analyzer trio: 0012 decides how findings are
> stored, 0014 how analyzers are executed and provisioned, and 0018 *which*
> analyzers are shipped and what each one actually covers — with the evidence,
> since 0012 and 0014 name `cargo-audit` and `semgrep` only as examples.

> **ADRs 0012–0015 form one decision set** and are best read together. They share a
> single structural rule: *knowledge that is not a derived/authored/inferred graph
> fact gets its own artifact store, and never borrows the graph's trust.* 0012, 0013
> and 0015 apply that rule to analyzer findings, to agent memory, and to
> generatively-produced media content; 0014 decides how analyzers are executed.
> Their execution sequence is [BUILD_PLAN_V2](../history/BUILD_PLAN_V2.md).
