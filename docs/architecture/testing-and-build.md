# Testing and build

## Required gates

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --no-deps
```

`make verify` runs the repository gates with the pinned Rust toolchain.
Frontend changes also run the frontend lint, unit tests, build, and mock
browser checks.

## Test layers

- Unit tests cover pure text, chunking, configuration, and provider behavior.
- Port contracts verify isolation, replay safety, graph behavior, and delete
  postconditions against the deterministic in-memory adapter.
- Integration tests exercise the public pipeline with deterministic providers.
- Retrieval evaluation measures path recall, fused ranking, and reranking.
- Live service checks are separate because they require Docker, Qdrant,
  FalkorDB, model files, and sometimes Ollama.

## Runtime prerequisites

Rust 1.95.0, Docker Compose, and the Rust toolchain are sufficient for the
default application. The pinned ONNX model downloads on first use into the
configured local model directory. Runtime data and Compose volumes are local
and must not be committed.
