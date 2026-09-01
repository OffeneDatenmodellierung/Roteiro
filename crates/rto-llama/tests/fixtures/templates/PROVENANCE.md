# Where these templates came from

Each `.jinja` here is a model's **own** `tokenizer.chat_template`, read out of
the GGUF the registry pulls and written to disk byte-for-byte. They are read by
`tests/real_chat_templates.rs`.

| Fixture | Registry model | GGUF it was read from |
|---|---|---|
| `qwen3-32b.jinja` | `qwen3-32b` | [`Qwen/Qwen3-32B-GGUF`](https://huggingface.co/Qwen/Qwen3-32B-GGUF) — `Qwen3-32B-Q4_K_M.gguf` |
| `qwen3-coder-30b-a3b.jinja` | `qwen3-coder-30b-a3b` | [`unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF`](https://huggingface.co/unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF) — `Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf` |
| `qwen3.8-27b.jinja` | `qwen3.8-27b` | [`ggml-org/Qwen3.8-27B-GGUF`](https://huggingface.co/ggml-org/Qwen3.8-27B-GGUF) — `Qwen3.8-27B-Q4_K_M.gguf` |

The `url` in `crates/rto-graph/src/models.rs` is the authority for each row; a
row here that disagrees with it is the row that is wrong.

To re-read one after a registry bump:

```rust
rto_llama::gguf::chat_template(&rto_graph::models::model_dir("<name>").join("model.gguf"))
```

## Why they are not in the vendored-dependency register

[`docs/VENDORED_DEPENDENCIES.md`](../../../../../docs/VENDORED_DEPENDENCIES.md)
exists for one reason, stated in its own opening: `cargo audit` and `cargo deny`
see Rust crates and not the C, C++ and assembly those crates vendor, so a
**native vulnerability** has somewhere it would be noticed. Its columns follow
from that — *Vendored inside*, *Reachable under*, *Advisories published at* —
and [ADR-0017](../../../../../docs/adr/0017-dependency-security-policy.md) §4 is
titled for it: "Native and vendored code is tracked by name".

These templates are none of that. Nothing links or compiles them, they ship in
no binary, and they are unreachable outside `cargo test`. There is no advisory
feed for a chat template, so the column the register exists to fill would be
empty — and a row whose security column is empty makes the table worse at the
job it has, which is telling you at a glance where a native CVE would surface.

What was genuinely missing is provenance, and that is this file. It is kept
beside the fixtures rather than in a central register because it is a fact about
these three files, and the person who needs it is the one looking at them.
