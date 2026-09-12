# Testing and build

## Required gates

Run these before a commit:

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --no-deps
```

`make verify` provides the repository equivalent. The repository CI workflow
runs these Rust gates with the pinned toolchain. Frontend changes also run the
package's lint, unit tests, build, and mock Playwright checks; Rust-backed live
checks remain a separate local/runtime-dependent suite.

## Test layers

- Unit tests cover pure text, chunking, configuration, and provider behavior.
- Port contracts run against every implementation and test postconditions such
  as collection isolation, error mapping, replay safety, and deletion cleanup.
- Integration tests use public library boundaries with deterministic providers
  and the real knowledge store where required.
- Evaluation tests measure retrieval paths and ranking; ignored real-model tests
  are separate because they download models and require network/runtime state.
- Frontend tests cover components, routes, API adapters, and browser smoke
  journeys. Mock browser coverage runs in CI; live Rust-backed coverage runs
  when the local model and native runtime prerequisites are available.

## Native and runtime prerequisites

The Ladybug dependency links OpenSSL and builds a bundled C++ engine. A C/C++
toolchain, CMake, and OpenSSL development libraries are required for a default
build. ONNX models are downloaded once into the configured model directory and
are then used offline. Runtime data is local, gitignored, and must never be
committed.
