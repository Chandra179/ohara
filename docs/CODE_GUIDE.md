# Ohara code guide

This guide applies to the five Rust packages and the local frontend. The
architecture source of truth is [ARCHITECTURE.md](ARCHITECTURE.md).

## Rust package rules

- The root `Cargo.toml` is workspace-only. There is no root `src/`.
- Each package owns its `Cargo.toml`, `src/`, implementation, tests, and
  Dockerfile.
- `main.rs` is a thin process boundary. Domain logic belongs in `lib.rs` or
  private modules behind it.
- Rust packages do not depend on one another. Cross-process coupling is the
  documented JSON artifact Interface under `docs/architecture/artifacts.md`.
- A module should have one responsibility, a small Interface, and high
  locality. Keep provider-specific code behind the process that owns it.
- The frontend has no filesystem, database, Docker, or provider knowledge.

## Style and API

Use the Rust Style Guide and Rust API Guidelines:

- `UpperCamelCase` for types, `snake_case` for functions/modules, and
  `SCREAMING_SNAKE_CASE` for constants.
- Prefer standard conversion traits and iterators over manual conversions.
- Keep fields private when invariants matter; use constructors that validate
  inputs once at the process Interface.
- Return `Result` at I/O, provider, and artifact seams. Use domain outcomes as
  values, not errors.
- Use `thiserror` enums with context. Do not hide errors in `String` when a
  structured variant is practical.
- Public items need rustdoc. Fallible public functions document `# Errors`.
- Write tests through the same Interface used by the process caller.

## Artifact rules

- Producers write temporary files and atomically rename them into an inbox.
- Consumers read only complete `.json` files and remove an input only after its
  output is durable.
- Document and chunk identities are deterministic hashes.
- Persisted JSON fields are part of the cross-team Interface; update producer,
  consumer, fixtures, and docs together.
- Do not put secrets, runtime data, model files, or database volumes in git.

## Lint policy

All packages inherit the workspace policy:

```toml
[lints.rust]
unsafe_code = "deny"
missing_docs = "warn"

[lints.clippy]
all = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
unwrap_used = "deny"
expect_used = "warn"
panic = "warn"
todo = "deny"
dbg_macro = "deny"
```

Warnings are errors in CI. Do not add broad lint exceptions to make a package
pass; improve the code or record a narrowly justified exception.

## Verification

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

Use `make verify` for the same Rust gates. The deterministic process-boundary
harness is run with `make pipeline-fixture`. Frontend changes must also pass
`npm run lint`, `npm test -- --run`, `npm run build`, and the mock browser suite
with `npm run e2e` from `frontend/`. The live browser suite is opt-in and uses
`npm run e2e:live` against a running stack.
