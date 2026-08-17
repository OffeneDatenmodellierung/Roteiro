---
Title: Configuration file — a single project-level TOML with layered precedence
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0007"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.2"
last-modified: 2026-08-16
confluence-url:
---

# ADR-0007: Configuration file — a single project-level TOML with layered precedence

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | MEDIUM |
| **Domain** | Developer Tooling |
| **Document version** | 1.2 |

## Reference

Introduces a **persistent configuration file** so per-project preferences (which models to use, what to ingest, inference thresholds, ignore paths, serving) are set once and shared, rather than re-passed as CLI flags every run. It gives a home to settings spread across [[docs/adr/0003-pluggable-embedding-models.md]] (model picks), [[docs/adr/0005-image-ocr-vision-ingestion.md]] (image ingestion toggles), and [[docs/adr/0006-local-model-serving.md]] (the `[serve]` table). Governed by the offline/deterministic principles of [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]].

## Summary

Add an optional **`roteiro.toml`** at the repository root (committed — so a team shares the same, reproducible settings), plus an optional user-level `~/.roteiro/config.toml`, with a clear precedence:

> **CLI flag > project `roteiro.toml` > user `~/.roteiro/config.toml` > built-in default.**

**One documented exception (v1.2).** For the remote-model-tier enable key this order is **inverted**: the project file may **deny but never grant**. See [[docs/adr/0019-remote-model-tier.md]]. The reason is in this ADR's own words — `roteiro.toml` is "committed — so a team shares the same, reproducible settings" — and a merged line authorising egress on every teammate's machine is not consent. Granting requires the **user** layer *and* the invocation, neither sufficient alone. Every other key follows the order above.

**Format: TOML, and only TOML.** Not YAML.

- TOML is the idiomatic Rust/Cargo choice and the `toml` crate is well-maintained.
- **`serde_yaml` is archived/unmaintained** (deprecated by its author in 2024) — adopting YAML would add a `cargo deny`/maintenance liability, and supporting two formats is redundant surface for no gain.

The file is **entirely optional** — every key has a working default, so Roteiro runs with no config at all; the file only overrides defaults. Unknown keys are ignored (forward-compatible), and a malformed file is a hard error (never a silent partial parse).

## Context

As Roteiro's surface has grown — a curated low/mid/high **model matrix** (ADR-0003), **ingestion toggles** (prose/PDF/OCR/vision, ADR-0005), inference/dedup thresholds, intent-debt ignore directives, and now **serving** (ADR-0006) — more behaviour is worth pinning *per project* rather than retyping as flags. A committed config also makes runs **reproducible and shareable**, which fits the dogfooded/deterministic ethos.

Forces to reconcile:

1. **Optional & defaulted (ADR-0001).** Roteiro must work with zero configuration; the file only ever *overrides* defaults. No config must never mean "broken."
2. **Reproducible & shareable.** A committed project file means a team gets identical behaviour; the user-level file is for personal, cross-project preferences (e.g. a default model tier for a beefy machine).
3. **One well-maintained format.** Rust's ecosystem standard is TOML; `serde_yaml` is unmaintained. One format, cleanly parsed, beats two.
4. **Flags still win.** A one-off `--min-confidence 0.6` must override the file, so precedence is CLI > project > user > default.

## Decision makers

- The Roteiro Project Team

## Recommended option

**Option 2 — a single optional `roteiro.toml` (+ user override), TOML-only, layered precedence (recommended).**

Initial schema (all keys optional; every section defaulted). Start with the high-value "sticky" ones (`[models]`, `[ingest]`, `[infer]`) and grow:

```toml
[models]                       # per-project overrides of the tier defaults (ADR-0003)
embedding  = "bge-base-en-v1.5"
generative = "qwen3-8b"

[ingest]                       # which content types feed meta.content, + caps (ADR-0005)
pdf = true
image_ocr = false
image_vision = false
max_content_chars = 1500

[infer]
min_confidence = 0.4
top_k = 5

[duplicates]
min_similarity = 0.9

[debt]
ignore = ["vendor/**", "**/generated/*"]   # paths excluded from intent-debt

[serve]                        # ADR-0006
models = false
addr = "127.0.0.1:8080"

[paths]
model_store = "~/.roteiro/models"   # today only the ROTEIRO_HOME env var
```

- **Loading:** parse user file, then project file, then apply CLI flags — each layer overriding the previous; a missing file is simply skipped; a present-but-malformed file is a hard error naming the offending key/line.
- **Scalars replace; the `[debt] ignore` exclusion list merges** (v1.1). Replace is right for a scalar — there is one `model`, one `addr`, and the nearer layer names it. It is wrong for an *exclusion* list, where the intent is nearly always additive: a user who globally ignores `vendor/**` and then adds one project-specific `thirdparty/**` wants both, and silently narrowing the union to the project's single pattern makes the tool report debt the user believed excluded, with nothing to indicate why. To inherit nothing instead, set `[debt] ignore_reset = true` — an explicit, all-or-nothing reset rather than a `!pattern` negation, because a mistyped negation removes nothing and says nothing, which is the same silent-wrong-answer failure the merge exists to fix. Only the exclusion list merges: `[workspace]`/`[standalone]` `roots`/`repos` are *discovery* lists (a merge would silently serve repos the project never named) and `[serve] models` is a *selection*, so both stay replace-wins.
- **A repository's own config governs how it is scanned, whoever is asking** (v1.1). When one process serves several repos (ADR-0008), each repo's settings are read from *that repo's* root — the same per-repo resolution ADR-0009 already uses for `[[links]]`. Otherwise repo B, scanned from repo A, is measured under A's exclusions and B's operators cannot explain the number they are shown.
- **Discovery:** project file is `roteiro.toml` at the repo root (found alongside the git dir); user file is `~/.roteiro/config.toml` (honouring `ROTEIRO_HOME`, consistent with the model store).
- **Feature-availability honesty:** a config key for an opt-in feature (e.g. `ingest.image_vision`) that the running binary was not built with produces a *warning*, not a silent no-op or a hard error — the setting is valid, the binary just can't honour it.

## Options considered + consequences

### Option 1: No config file — CLI flags + env only (status quo)
- Pros: nothing to build; fully explicit.
- Cons: sticky per-project preferences must be retyped every run; no shareable, reproducible project settings; env vars are a poor home for structured config. Rejected as the surface grows.

### Option 2: A single optional TOML file with layered precedence (recommended)
- Pros: reproducible/shareable (committed project file); idiomatic, well-maintained format; optional and fully defaulted; flags still win; a natural home for model picks, ingestion, serving. 
- Cons: a new parse path + precedence logic to get right (mitigated: small, well-tested, `toml` + serde); another place settings can live (mitigated: strict precedence + a `roteiro config` command can print the effective, merged config).

### Option 3: Support both TOML and YAML
- Pros: familiarity for YAML users.
- Cons: `serde_yaml` is archived/unmaintained (deny/maintenance risk); two formats double the surface and invite drift for no real benefit. Rejected — TOML only.

## Consequences

- A new optional `roteiro.toml` (project, committed) and `~/.roteiro/config.toml` (user); zero-config still works — the file only overrides defaults. Adds the `toml` crate (permissive licence, `cargo deny`-clean) to the binary.
- Precedence is fixed and documented: **CLI > project > user > default**. A `roteiro config` command prints the effective merged configuration and its provenance (which layer set each value), so "why did it use that model?" is answerable. For the merging `[debt] ignore` list, provenance is reported **per pattern** rather than per key — a single label would misreport a list holding patterns from both layers — and an `ignore_reset` prints the inherited patterns it discarded, so the reset cannot hide what the merge was added to reveal.
- Settings currently only reachable by flag/env gain a persistent home: model picks (ADR-0003), ingestion toggles + caps (ADR-0005), inference/dedup thresholds, intent-debt ignore paths, the `[serve]` table (ADR-0006), and the model-store path (today only `ROTEIRO_HOME`).
- Forward-compatible: unknown keys are ignored; a malformed file fails loudly (never a silent partial parse). A key for a feature the binary lacks warns rather than errors.

## Advice Received

Project direction incorporated: add a config file, but keep it **optional and fully defaulted** (no config must never break Roteiro), make it **committed and reproducible** at the project level with a personal user-level override, and use **one well-maintained format — TOML, not YAML** (since `serde_yaml` is unmaintained) — with CLI flags always winning.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.2 | 2026-08-17 | Amended by [[docs/adr/0019-remote-model-tier.md]]. One key — the remote-model-tier enable — inverts the precedence: the committed project file may **deny but never grant** egress, and granting needs the user layer plus the invocation. Recorded here as well as in 0019 because a reader of this ADR would otherwise apply the general rule and be wrong. No other key is affected. Also corrects the header table, which read 1.0 while the frontmatter read 1.1. |
| 1.1 | 2026-08-16 | Amended (issue #321). Two refinements to layering, neither changing the CLI > project > user > default order: (a) **list-valued exclusion keys merge** — `[debt] ignore` unions the layers instead of the project layer discarding the user layer, with a new `ignore_reset` key as the explicit way to inherit nothing, and per-pattern provenance in `roteiro config`; discovery/selection lists deliberately still replace. (b) **Per-repo resolution**: in a multi-repo process each repository is scanned under its *own* config, extending ADR-0009's per-repo `[[links]]` rule. Motivation: the graph API applied no exclusions at all, so the explorer UI and the CLI reported different intent debt for the same repository. |
| 1.0 | 2026-08-09 | Accepted. Optional `roteiro.toml` (project, committed) + `~/.roteiro/config.toml` (user), TOML-only (YAML rejected — `serde_yaml` unmaintained), precedence CLI > project > user > default. Initial schema: `[models]`/`[ingest]`/`[infer]` first, then `[duplicates]`/`[debt]`/`[serve]`/`[paths]`. Fully defaulted (zero-config works); unknown keys ignored; malformed = hard error; missing-feature keys warn. |
