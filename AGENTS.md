# AGENTS.md

Ohara is a Rust monorepo with five independent packages: `scraper`,
`cleaning`, `indexer`, `graph`, and `retrieval`. The root `Cargo.toml` is
workspace-only; there is no root `src/`. `frontend/` is a local React/Vite
application and is not containerized.

## Docs of record

- `docs/ARCHITECTURE.md` — system design and process ownership.
- `docs/architecture/` — process Interfaces and artifact contracts.
- `docs/CODE_GUIDE.md` — Rust/frontend style and verification policy.
- `CONTEXT.md` — domain vocabulary and decisions.
- `TODO.md` and `frontend/TODO.md` — prioritized open work.

## Verification

Run the pinned Rust toolchain through the Makefile:

```text
make verify
```

This runs formatting, Clippy with warnings denied, workspace tests, and docs.
Frontend gates are run from `frontend/` with `npm run lint`, `npm test -- --run`,
and `npm run build`.

## Architecture rules

- Each package owns one process responsibility and its own `Cargo.toml`, `src/`,
  tests, and Dockerfile.
- Rust packages do not depend on one another. Cross-process coupling is the
  JSON artifact Interface documented in `docs/architecture/artifacts.md`.
- `scraper` owns network fetches and raw artifacts.
- `cleaning` owns article extraction and quality normalization.
- `indexer` owns canonical chunking, embeddings, and Qdrant writes.
- `graph` owns entity/mention extraction and FalkorDB writes.
- `retrieval` owns frontend HTTP, read projections, Qdrant retrieval, and
  Ollama synthesis.
- Producers write temporary files and atomically rename them into inboxes.
- Deterministic document and chunk ids make replay idempotent.
- Runtime data under `data/` is ignored and must never be committed.
- Keep provider-specific types inside the package that owns that provider.
- Return `Result` at I/O and provider seams; document errors on public APIs.
- Do not add broad lint exceptions; fix the code or justify a narrow exception.
