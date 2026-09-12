# AGENTS.md

Single-crate Rust workspace (edition 2024, Rust 1.95+): `lib.rs` holds library logic and `main.rs` is the binary command boundary for the worker and operator commands (`ohara --config <path>`, `query`, backups, lifecycle, metrics, and ER merge).

## Docs of record (read before editing)
- `docs/ARCHITECTURE.md` — the design source of truth (§ numbers cited everywhere in code).
- `docs/CODE_GUIDE.md` — style + lint policy (C-* guidelines), §7 has the CI gates.
- `TODO.md` — what is NOT implemented yet. Notably: cloud `Llm` providers, embedding dual-write migration, Symspell/HyDE evaluation, stage throughput dashboards, HNSW, and threshold measurement remain open. These are tracked, not broken — don't "fix" them as bugs.

## Build prerequisites (non-obvious)
- `lbug` links OpenSSL at link time → `sudo apt install libssl-dev` (README §Building has a no-sudo `OPENSSL_DIR` workaround) and compiles its bundled C++ engine on first build (CMake + C++ toolchain, ~2 GB scratch).
- ONNX models (`bge-small-en-v1.5`, reranker) are fetched from the HF hub on first use into the configured models dir; offline afterwards. Real-model eval caches under `~/.cache/ohara-test`.

## Verification (gates from CODE_GUIDE §7; CI runs these on every change)
```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --no-deps
```
Focused runs:
- Stage/module tests: `cargo test --lib pipeline::retrieve` (or any module path).
- Integration suites are `#[path]` modules mounted in `tests/integration.rs`; run one via `cargo test --test integration eval`.
- The real-model golden-set eval is `#[ignore]`d (downloads models): `cargo test --test integration eval_retrieval_baseline_real_models -- --ignored`. The hermetic `eval_retrieval_baseline_machinery` runs normally.
- `tests/ports/` contains the port contract suites; keep them updated for every
  new implementation or contract change.

## Lint discipline (will trip you up)
- `unwrap_used = deny` across the codebase; tests opt out via a file-level `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` header.
- `expect_used = warn` — only with the invariant stated in the message (§10); `panic = warn`; `todo = deny` (never leave `todo!()`); `dbg_macro = deny`; public API needs doc comments (`missing_docs = warn`).
- The only clippy pedantic allowance: `module_name_repetitions` (justified in Cargo.toml). Don't add more.

## Architecture rules (verified, not guessable from filenames)
- Three planes, each the sole owner of its datastore: `control/` = all SQLite, `knowledge/` = all LadybugDB, `engine/` = all networking. `pipeline/` depends only on port traits (`Fetcher`, `KnowledgeStore`, `Embedder`, `Reranker`, `Llm`, `Extractor`, `QueryNormalizer`) re-exported through the plane facades. A PR that names a vendor type (`lbug`, `fastembed`, `rusqlite`, …) outside its owning plane is rejected.
- Heavy native stacks are feature-gated: `default = ["ladybug", "onnx-embedder"]`. `LocalEmbedder`/`LocalReranker` are `#[cfg(feature = "onnx-embedder")]`. `--no-default-features` is for deployments injecting remote providers through `Worker::with_ports`.
- `data/` is gitignored — runtime system of record, never commit it.
- Identity discipline: immutable rows are content-hash keyed (`chunk_id = sha256(doc_id:seq)`, triplets likewise); entities use uuidv7 surrogates + `UNIQUE(canonical_name, entity_type)`. Writes are idempotent upserts (`ON CONFLICT DO UPDATE`, never `INSERT OR REPLACE`) — replay/retry must be safe.
- Chunk/triplet/vector writes must keep SQLite and LadybugDB consistent; there is no shared transaction — follow the §7 protocol.
- Commit messages reference build order, e.g. `…(§15 step 4)`.

## Conventions
- Backyard module style (ch07): `pub mod` planes at the crate root, private leaf modules, facade re-exports, absolute `crate::` paths. Ports live with their consumer or owner: `Embedder` in `pipeline/embed.rs`, `Reranker`/`QueryNormalizer` in `pipeline/retrieve.rs`, `Extractor` in `pipeline/clean.rs`, `Fetcher` in `engine.rs`, `KnowledgeStore` in `knowledge.rs`, `Llm` in `llm.rs`.
- Errors: `Result` at every boundary; domain outcomes (paywalled/duplicate/low-quality/wrong-language) are values, not errors. Port errors carry a retry `Class` driving the job state machine. Worker isolates panics with `catch_unwind`.
