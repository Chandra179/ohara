Create two documentation artifacts in /home/koala/Work/ohara — docs only, no source code:

1. **README.md** (repo root)
   - One-paragraph description: ohara — embedded, zero-daemon web scraping → clean text → chunking → GraphRAG pipeline in Rust
   - Tech stack table (Obscura, SQLite/WAL, readability/html2md, whatlang/symspell, LadybugDB, FlashRank, pinned local embedder)
   - Pipeline stage diagram (Stages 1–5)
   - Full directory tree (ch07-compliant: lib.rs crate root, `pub mod` planes, private leaf modules, tests/, migrations/, data/, docs/)
   - Module-by-module explanation (what + why): control, engine, knowledge, pipeline + stages, llm, text, config
   - Conventions: visibility discipline, ports at plane boundaries, dependency direction
   - Status/roadmap + link to docs/ARCHITECTURE.md

2. **docs/ARCHITECTURE.md** — updated system design (v2)
   - Three-plane architecture + module map
   - Embedding layer (pinned ONNX embedder, tokenizer-aligned chunking, per-batch model versioning, re-embed migration)
   - Cross-store consistency policy: deterministic IDs, intent-before-write, boot-time reconciliation, re-chunk/re-crawl delete policies
   - SQLite schema v2: amended documents, jobs (lease claim), chunks, entities, entity_aliases, stage_events, schema_migrations
   - Stage specs 1–5 hardened (fetch ladder, chunking, graph construction, retrieval with FTS5 third path, top-50→rerank→top-5)
   - Ports & substitution (LSP): per-port contracts as postconditions, capability declarations, swap candidates
   - **Error handling section (ch09-grounded):**
     - Policy: Result at all boundaries (all boundary failure is expected per ch09-03), panic only for broken internal invariants, domain outcomes (quality rejection, duplicate) as values not errors
     - Layered taxonomy: port error enums (thiserror) with retry classification; `From` mapping via `?`; per-impl error-mapping tests in tests/ports/
     - StageError {Transient, Permanent, Fatal} → job transitions (RETRY/FAILED/DEAD) + audit of every error to stage_events
     - Worker-loop catch_unwind isolation, panic=unwind profile rationale, boot-time reconciliation on panic
     - Newtype validation (DocumentId, ChunkId, CanonicalName, NormalizedUrl) — parse-don't-validate per ch09-03 Guess pattern
     - thiserror (library) / anyhow (binary) split; main → Result with non-zero exit; clippy unwrap_used deny; expect-with-invariant-message rule
   - Contract-test strategy, eval harness, robots.txt/politeness policy, security & governance notes
   - Appendix: module conventions and workspace graduation path