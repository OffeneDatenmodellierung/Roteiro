# Architecture

Roteiro assembles a provenance-tagged knowledge graph of this codebase, so
humans and agents can query one surface that mixes precise code facts with
authored intent. The load-bearing pieces are below; see how facts are labelled
in [[provenance#Authored layer]].

## Graph store

The graph lives in a single SQLite database behind
[[crates/rto-graph/src/store.rs#Store]]. It is a set of nodes and
provenance-tagged edges; a code-changing sync replaces the derived graph
atomically, while imported and authored layers are re-applied on top.

## Sync engine

[[crates/rto-graph/src/sync.rs#sync]] brings the store into agreement with the
committed `HEAD` tree. Extraction is content-addressed by blob id, so only blobs
whose content changed are re-parsed; the rest load from the object cache.

## Extraction

[[crates/rto-graph/src/extract.rs#RustExtractor]] turns a Rust source blob into
symbol nodes (functions, structs, traits, …) and structural edges using
tree-sitter. Cross-file call resolution happens later, at assembly time.
