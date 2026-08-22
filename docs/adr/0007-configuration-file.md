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
version: "1.5"
last-modified: 2026-08-21
confluence-url:
---

# ADR-0007: Configuration file — a single project-level TOML with layered precedence

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | MEDIUM |
| **Domain** | Developer Tooling |
| **Document version** | 1.5 |

## Reference

Introduces a **persistent configuration file** so per-project preferences (which models to use, what to ingest, inference thresholds, ignore paths, serving) are set once and shared, rather than re-passed as CLI flags every run. It gives a home to settings spread across [[docs/adr/0003-pluggable-embedding-models.md]] (model picks), [[docs/adr/0005-image-ocr-vision-ingestion.md]] (image ingestion toggles), and [[docs/adr/0006-local-model-serving.md]] (the `[serve]` table). Governed by the offline/deterministic principles of [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]].

## Summary

Add an optional **`roteiro.toml`** at the repository root (committed — so a team shares the same, reproducible settings), plus an optional user-level `~/.roteiro/config.toml`, with a clear precedence:

> **CLI flag > project `roteiro.toml` > user `~/.roteiro/config.toml` > built-in default.**

**One rule inverts this order (v1.4).** For a **capability key** — one whose effect is to *turn something on* whose cost or risk falls on whoever runs the command — the project file may **deny but never grant**.

This began as a single documented exception for the remote tier in v1.2. It is now the standard, because an exception list grows one entry at a time until it *is* the rule and nobody has noticed. A rule with a stated scope can be applied to a new key by whoever adds it; an exception list can only be extended by whoever remembers it exists.

The reason is in this ADR's own words: `roteiro.toml` is "committed — so a team shares the same, reproducible settings". That is exactly right for a *setting* and exactly wrong for a *permission*. A merged line that starts sending source elsewhere, or running builds, on every teammate's machine is consent by pull request — granted by someone else, noticed by nobody. The project layer may switch such a capability **off** for everyone, and may never switch it **on** for anyone.

### Which keys this covers

A key is a capability key if setting it causes something to happen that otherwise would not, and that thing does at least one of:

1. sends repository content **off the machine**;
2. **executes code the repository supplies**, rather than code Roteiro ships;
3. **writes outside** the repository and Roteiro's own caches, *that would not otherwise occur* — Roteiro already writes to `~/.roteiro` by default, so a key that changes **where** rather than **whether** does not qualify;
4. spends **materially more of the machine** than an ordinary command — loading a multi-gigabyte model, or running a build;
5. **removes a guard** against any of the above. A guard is disabled by setting something to `false`, so this clause exists because such a key is *a grant wearing a denial's grammar* and would otherwise have to be caught by noticing that one of 1–4 applies transitively.

**A capability key's built-in default is denied.** If a key defaults to *granted*, then setting it in a project file causes nothing that would not otherwise happen — it fails clause 1 of the test before any of 1–5 is reached — and it is a value. Whatever *does* gate the behaviour is the real capability, and it is that which the rule governs. This is not a caveat; it is how three of the keys below are classified.

Everything else is a **value**, and values follow the ordinary order above. Most keys are values, and for them the inversion is not merely unnecessary but *inexpressible*: there is no "deny" for `[models] generative = "qwen3-8b"`, only a different value.

| Key | Class | Why |
|---|---|---|
| `[remote] enabled` | **capability** | test 1 — [[docs/adr/0019-remote-model-tier.md]] §3 |
| host execution for builders | **capability** | test 2 — [[docs/adr/0020-build-capable-sandboxed-execution.md]] §6 |
| `[media] gate` | **capability** | test 5, and test 4 beneath it. `gate = false` "sends every blob to the model", so a project setting it spends every teammate's machine on model runs that would not otherwise happen, and admits the confabulation [[docs/adr/0015-generated-media-content-artifact-store.md]] exists to prevent |
| `[ingest] ocr`/`vision`/`audio` | value | **corrected on inspection.** Each defaults to `true`, so a project setting `true` grants nothing that is not already granted. What actually gates them is the **build feature** — `image-ocr`, `image-vision`, `audio-transcribe`, none of them default — which is a stronger gate than a config default, being a decision taken at install. Should any default ever flip to `false`, these become capability keys and this row is wrong |
| `[ingest] prose`/`pdf` | value | no model and no repository-supplied code: parsing, which is what the rest of extraction already does |
| `[paths] model_store`, `[telemetry] file` | value | test 3 as sharpened above. These change **where** Roteiro writes, not **whether** — it writes to `~/.roteiro` by default regardless |
| `[models]`, `[debt]`, `[serve]`, `[infer]`, `[duplicates]`, `[workspace]`, `[[links]]`, `[pins]`, `[telemetry] rotation`/`format`, `[media] silence_rms`/`image_variance` | value | settings, not permissions |
| `[mcp] tools` | value — but see below | test 1. The default already advertises every tool, so a project file setting it causes nothing that would not otherwise happen. It is nonetheless the **first key whose every setting is a denial**, and its layers therefore intersect rather than override (v1.5) |

Two of those rows are worth reading as method rather than as answers. `[ingest]` was expected to be a capability and is not, because its default already grants — which is what produced the default rule above, and is a reminder that a key's *class* cannot be read off its subject matter. `[media] gate` is the reverse: an unremarkable-looking boolean that turns out to be the only key here whose **`false` is the dangerous value**.

### A third case the classification did not have a name for (v1.5)

`[mcp] tools` restricts what an MCP server advertises. It is a **value** by this
ADR's own default rule — the built-in surface is every tool, so naming a subset
in a committed `roteiro.toml` grants nothing that was not already granted, and
the capability test fails at its first clause. But applying the ordinary
precedence to it would have been wrong, and the reason is worth recording
because the abstract rule did not reach it.

Every setting of the key is a **denial**. Naming a tool does not ask for that
tool; it declines every tool not named. So "the nearer layer wins" lets a nearer
layer *un-deny*: a committed project file could restore `sandbox_clear` after
the machine's owner removed it in `~/.roteiro/config.toml`. That is exactly the
outcome v1.2's inversion exists to prevent, reached from the other direction —
not by a project file granting a capability, but by a project file cancelling a
denial.

The rule is therefore: **a key whose every setting is a denial layers by
intersection.** Every layer may narrow, none may widen, the invocation included.
The flag narrows rather than winning because a flag has nothing else to express
here — there is no grant available to it, exactly as this ADR says of a value
key that "there is no deny for `[models] generative`, only a different value" —
and because under issue #579 Roteiro may write its own `roteiro mcp …` line into
a third-party agent's config, so the invocation is not reliably a person at a
prompt while the user layer is unambiguously that person's standing intent.

Intersection is a lattice meet: order-independent, idempotent and monotone
downwards, so "may deny, may not grant" holds by construction rather than by a
rule each call site remembers — which is what the section below requires. An
empty intersection is a **startup error**, never an unrestricted server: a
restriction that quietly restricts nothing is the defect the key was added for
(issue #584, and omnigent#5178 on the client side of the same wire).

The key sits in its own `[mcp]` table rather than in `[serve]` because
`[serve] tools` is taken and means something else — a boolean deciding whether
the served-chat model is offered the graph tools at all. Two keys called `tools`
in one table, over two different surfaces, would be worse than a second table.

### The mechanism is structural, not remembered

`RemoteConfig` implements the inversion with a bespoke `overlaid_with`. A second bespoke implementation is how a rule decays into a convention, and a third is how a convention decays into folklore. A capability key's layering must therefore be carried by its **type**, so that declaring a key a capability and getting its precedence right are *the same act* rather than two things a future author has to remember to do together — the same reasoning that put debt exclusions behind one function and truncation behind one `window`.

`roteiro config` reports a key's class beside its layer, because a reader who sees one key inherit and its neighbour refuse to cannot otherwise tell whether that is the rule or a bug.

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
embedding  = "bge-base-en-v1.5"     # `infer`
generative = "qwen3-8b"             # `spec draft` and the Ask panel
vision     = "smolvlm-500m-gguf"    # `media build`, image description   (v1.3)
audio      = "voxtral-mini-3b"      # `media build`, speech transcription (v1.3)
ocr        = "ocrs-text"            # `sync`, literal text in images      (v1.3)

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
- **A key whose *value* is wrong is a different thing, and it fails loudly (v1.3).** The warn rule above is about a
  build that lacks a feature: the setting is correct and this binary cannot act on it. A `[models]` key naming a model
  that does not exist, or one of the wrong modality, is not that — it is wrong everywhere, and no rebuild fixes it. Those
  are **named errors quoting the key**, and never a fall-back to the default: a fall-back would leave the file appearing
  honoured while a different model ran, and on the llama.cpp path a model of the wrong architecture does not
  mis-answer, it aborts the process (`GGML_ASSERT`). The single exception is `roteiro config` itself, which *reports*
  the bad key rather than refusing — it is the command an operator runs precisely because a pin is not doing what they
  expected, so it must not be the one command the pin stops.

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
- Precedence is fixed and documented: **CLI > project > user > default**. A `roteiro config` command prints the effective merged configuration and its provenance (which layer set each value), so "why did it use that model?" is answerable. **Per *surface*, since v1.3**: the `[models]` keys alone stopped being an answer once six surfaces consumed five keys, two of them sharing one — so `roteiro config` also prints the resolved model for `embed`, `draft`, `chat`, `transcribe`, `describe` and `ocr`, each with the rule that chose it and, for a pin, the layer it came from. The same reasoning as the per-pattern `[debt] ignore` provenance below: report what the operator observes, not only the input it was derived from. For the merging `[debt] ignore` list, provenance is reported **per pattern** rather than per key — a single label would misreport a list holding patterns from both layers — and an `ignore_reset` prints the inherited patterns it discarded, so the reset cannot hide what the merge was added to reveal.
- Settings currently only reachable by flag/env gain a persistent home: model picks (ADR-0003), ingestion toggles + caps (ADR-0005), inference/dedup thresholds, intent-debt ignore paths, the `[serve]` table (ADR-0006), and the model-store path (today only `ROTEIRO_HOME`).
- Forward-compatible: unknown keys are ignored; a malformed file fails loudly (never a silent partial parse). A key for a feature the binary lacks warns rather than errors.

## Advice Received

Project direction incorporated: add a config file, but keep it **optional and fully defaulted** (no config must never break Roteiro), make it **committed and reproducible** at the project level with a personal user-level override, and use **one well-maintained format — TOML, not YAML** (since `serde_yaml` is unmaintained) — with CLI flags always winning.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-09 | Accepted. Optional `roteiro.toml` (project, committed) + `~/.roteiro/config.toml` (user), TOML-only (YAML rejected — `serde_yaml` unmaintained), precedence CLI > project > user > default. Initial schema: `[models]`/`[ingest]`/`[infer]` first, then `[duplicates]`/`[debt]`/`[serve]`/`[paths]`. Fully defaulted (zero-config works); unknown keys ignored; malformed = hard error; missing-feature keys warn. |
| 1.1 | 2026-08-16 | Amended (issue #321). Two refinements to layering, neither changing the CLI > project > user > default order: (a) **list-valued exclusion keys merge** — `[debt] ignore` unions the layers instead of the project layer discarding the user layer, with a new `ignore_reset` key as the explicit way to inherit nothing, and per-pattern provenance in `roteiro config`; discovery/selection lists deliberately still replace. (b) **Per-repo resolution**: in a multi-repo process each repository is scanned under its *own* config, extending ADR-0009's per-repo `[[links]]` rule. Motivation: the graph API applied no exclusions at all, so the explorer UI and the CLI reported different intent debt for the same repository. |
| 1.2 | 2026-08-17 | Amended by [[docs/adr/0019-remote-model-tier.md]]. One key — the remote-model-tier enable — inverts the precedence: the committed project file may **deny but never grant** egress, and granting needs the user layer plus the invocation. Recorded here as well as in 0019 because a reader of this ADR would otherwise apply the general rule and be wrong. No other key is affected. Also corrects the header table, which read 1.0 while the frontmatter read 1.1. |
| 1.3 | 2026-08-17 | Amended (Stage 33). **`[models]` grows from two keys to five**: `vision`, `audio` and `ocr` join `embedding` and `generative`, one key per model *kind* rather than per command (`generative` governs both `spec draft` and Ask). Until now those three models were compiled-in constants, so a project could **not pin its ASR model at all** — the setting did not exist. Two rules are recorded here because a reader would otherwise apply the general ones and be wrong: (a) a key whose *value* is wrong — unknown model, wrong modality — is a **named error quoting the key**, not the *warning* that a missing-feature key gets, and never a silent fall-back to the default; (b) `roteiro config` reports such a key instead of refusing, being the command an operator runs when a pin is misbehaving. `roteiro config` also gains a **per-surface resolution table** (model, rule, layer, installed), on the same reasoning as the per-pattern `[debt] ignore` provenance added in v1.1. Precedence is unchanged; unset resolves to exactly the models each surface used before. |
| 1.4 | 2026-08-19 | Amended on the owner's ruling that v1.2's inversion should be the standard rather than an exception, and that every key be classified under it. The project file may **deny but never grant** any **capability key** — one that turns on something whose cost or risk falls on whoever runs the command — with a four-part test (sends content off the machine; executes repository-supplied code; writes outside the repository; spends materially more of the machine) so a new key can be classified by whoever adds it rather than by whoever remembers the exception list. Records that most keys are **values**, for which the inversion is not merely unneeded but inexpressible. Classifying the whole surface produced two additions the abstract rule had missed: a fifth clause for a key that **removes a guard** (a grant wearing a denial's grammar — `[media] gate = false`, the only key here whose dangerous value is `false`), and the rule that **a capability key's built-in default is denied**, without which a key that already defaults to granted looks like a capability while being unable to grant anything. That rule reclassified `[ingest] ocr`/`vision`/`audio` from capability to value: their real gate is the non-default **build feature**, not the config key. Test 3 sharpened to writes *that would not otherwise occur*, since Roteiro writes to `~/.roteiro` regardless and `[paths]`/`[telemetry]` change where rather than whether. Also requires the mechanism be **structural** — carried by the key's type rather than by a bespoke `overlaid_with` per capability — because a second hand-written inversion is how a rule decays into a convention. |
| 1.5 | 2026-08-21 | Amended (issue #584). Adds `[mcp] tools`, the advertised MCP tool surface, and with it a case v1.4's classification had no name for: a key that is a **value** by the default rule — its default already advertises everything, so a project file grants nothing new — and whose **every setting is nonetheless a denial**. Ordinary precedence would let a nearer layer *un-deny*, restoring a tool the machine's owner removed, which is v1.2's failure reached from the other direction. Such a key therefore **layers by intersection**: every layer may narrow the surface and none may widen it, the invocation included, and the flag narrows rather than winning because it has no grant to express and is not reliably written by a person (issue #579). Intersection is a lattice meet, so the property holds by construction rather than by a remembered rule, satisfying v1.4's requirement that the mechanism be structural. An empty intersection is a startup error and never an unrestricted server. `roteiro config` labels the key `project ∩ user` rather than naming a winning layer, on the same reasoning as v1.1's per-pattern `[debt] ignore` provenance. |
