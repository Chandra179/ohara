# ohara — Code Guide

> How code is written in this repo: naming, API design, documentation, formatting, and lint policy. Distilled from the **[Rust API Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html)** and the **[Rust Style Guide](https://doc.rust-lang.org/nightly/style-guide/)**, applied to ohara's architecture ([ARCHITECTURE.md](ARCHITECTURE.md)) and module conventions (Rust Book ch07) — not a replacement for either source.

**What governs what:**

| Concern | Governing document | Enforced by |
| :--- | :--- | :--- |
| Formatting | [Rust Style Guide](https://doc.rust-lang.org/nightly/style-guide/) | `cargo fmt` — never hand-format |
| Lints | This guide's `[lints]` table (C-LINT below) | `make clippy`, warnings = errors |
| API shape & naming | [API Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html) | code review, this guide's checklist |
| Module tree, visibility | Rust Book ch07, ARCHITECTURE Appendix A | compiler + review |
| Errors, panics | Rust Book ch09, ARCHITECTURE §10 | compiler + contract tests |
| Design decisions | ARCHITECTURE.md (v2.6) | — |

---

## 1. Formatting — rustfmt's job

`cargo fmt` is the only formatter; `make fmt-check` runs `cargo fmt --check` with the pinned toolchain from [`rust-toolchain.toml`](../rust-toolchain.toml). No project `rustfmt.toml` — the defaults **are** the [default Rust style](https://doc.rust-lang.org/nightly/style-guide/):

- Spaces, **4-space indent**, **max line width 100**.
- **Block indent over visual indent** (smaller diffs, less rightward drift):

  ```rust
  // yes
  worker.claim_job(
      stage,
      priority,
  );
  // no
  worker.claim_job(stage,
                   priority);
  ```

- **Trailing commas** in any comma-separated list followed by a newline (diff- and move-friendly).
- Zero or one blank line between items/statements; no trailing whitespace anywhere.
- Comments: prefer `//` over `/* */`, one space after the sigil, comment-only lines ≤ 80 chars, complete sentences. Doc comments: `///` outer comments, placed **before** attributes; `//!` only for crate/module-level docs.
- One `derive` attribute, one attribute per line.
- Where the style guide says "sort" (imports, struct fields, derives), that means **version-sorting** — `u8 < u16 < u128`, `_` sorts as a word separator. rustfmt does it; don't fight it in review.

## 2. Naming — RFC 430 casing and conversion idioms

- **C-CASE:** `UpperCamelCase` types/traits/enums, `snake_case` functions/methods/modules/files, `SCREAMING_SNAKE_CASE` consts/statics. ohara examples: `KnowledgeStore`, `NormalizedUrl`, `ChunkFilter`, `control/db.rs`.
- **C-CONV:** ad-hoc conversions follow the as/into ladder — `as_` (cheap reference view), `to_` (cheap-ish copy/owned view), `into_` (consuming). Standard traits always preferred: implement `From`/`AsRef`/`AsMut` (C-CONV-TRAITS) and let `?`, `.into()`, `.as_ref()` do the work. ohara's `NormalizedUrl`, `DocumentId`, `ChunkId` newtypes implement `From<&str>`/`From<String>` and `AsRef<str>`.
- **C-GETTER:** getters drop the `get_` prefix — `model_id()`, `capabilities()`, `class()` (§9 ports), not `get_model_id()`.
- **C-ITER / C-ITER-TY:** collection-producing-iterator methods are `iter`, `iter_mut`, `into_iter`; the iterator types they return are named after the method.
- **C-WORD-ORDER:** consistent error/enum naming — `FetchError`, `KnowledgeError`, `StageError` (noun + `Error`); predicate methods `is_*`/`has_*` (C-PRED); constructors are static inherent methods named `new` (C-CTOR) — `NormalizedUrl::new(raw, final_url) -> Result<Self, …>`.
- **C-FEATURE:** cargo feature names describe content, never placeholders: `ladybug`, `onnx-embedder`, and the future `obscura` feature — never `extra`, `new`, `full`.

## 3. API design

- **C-COMMON-TRAITS:** types eagerly derive `Clone`, `Debug`, and whatever applies (`Eq`/`Hash` for ID newtypes, `Default` for config where sane). **C-DEBUG:** every public type implements `Debug`, and `Debug` output is never empty (C-DEBUG-NONEMPTY).
- **C-SEND-SYNC:** everything crossing a port or the worker loop is `Send + Sync` — this is a hard requirement for all port traits (§9), not an aspiration.
- **C-OBJECT:** port traits must stay object-safe (`dyn`-compatible) — that's why they use `#[async_trait]` and have no generic methods (§9). If a trait may be useful as a trait object, object-safety is part of its contract.
- **C-SEALED:** traits whose impl set is closed (e.g. internal taxonomy traits) are sealed — downstream impls would break LSP verification.
- **C-GOOD-ERR:** error types are `enum`s with thiserror, carry context (`AntiBot { url }`, `Timeout { secs }`), implement `Error` + `source()` chains, and never stringly-typed unless the payload is genuinely free-form (`Protocol(String)`). Every port error exposes `class() -> Class` (§10).
- **C-VALIDATE + C-NEWTYPE:** validation happens once at the boundary through newtypes (`NormalizedUrl`, `CanonicalName`, `TokenBudget`) — parse, don't validate (ch09-03). Constructors return `Result`; downstream code sees only valid values.
- **C-CUSTOM-TYPE:** no `bool` or `Option<…>` parameters where a type would say more — escalation triggers are an enum, not two bools; `FetchCapabilities { js_rendering, stealth }` over loose flags.
- **C-METHOD / C-NO-OUT:** methods for things with a clear receiver; no out-parameters — return values or structured results (`CleanOutcome` is a value, not a `&mut` write-back).
- **C-STRUCT-PRIVATE:** struct fields are private; construction goes through constructors or builders. **C-BUILDER:** `config.rs` builds the immutable `Config` through a builder/fallible constructor that fails fast at boot (§10).
- **C-INTERMEDIATE / C-CALLER-CONTROL:** stages expose intermediate results (e.g. chunking returns chunks before embedding; embedding returns vectors before the knowledge-plane write) so the pipeline orchestrates instead of each stage owning the world. Functions take data by reference where the caller keeps it, by value where it consumes it.

## 4. Documentation

- **C-CRATE-DOC:** `lib.rs` opens with a crate-level `//!` overview and at least one `# Examples` block exercising the public API end-to-end (enqueue → complete, against the control plane).
- **C-EXAMPLE:** new public facade items should get a rustdoc example when the setup is useful to callers; examples use `?`, never `unwrap`/`try!` (C-QUESTION-MARK).
- **C-FAILURE:** function docs carry `# Errors` (which variants, when) and `# Panics` sections whenever either applies. In ohara, `# Panics` documents *invariant* panics only — any other failure mode is a `Result` (§10).
- **C-LINK:** prose doc comments hyperlink types and sections (`[`Fetcher`](crate::engine::Fetcher)`); rustdoc links are checked by `cargo doc`.
- **C-HIDDEN:** implementation details stay out of docs — vendor types never appear in public signatures (§9 rule 3), so rustdoc never leaks them.
- **C-METADATA:** `Cargo.toml` carries authors, description, license, repository, keywords before first publish.

## 5. Lint policy (C-LINT)

Enforced from `Cargo.toml` so binaries, the library, and tests inherit it:

```toml
[lints.rust]
unsafe_code = "deny"          # ohara needs no unsafe; raw HTML is data (§12)
missing_docs = "warn"         # public API must be documented (C-EXAMPLE)

[lints.clippy]
all       = { level = "warn", priority = -1 }
pedantic  = "warn"            # the API-guidelines spirit, mechanically checked
unwrap_used = "deny"          # ch09 policy — §10
expect_used = "warn"          # allowed only with the invariant stated in the message
panic       = "warn"          # deliberate invariant panics must be visibly reviewed
todo        = "deny"
dbg_macro   = "deny"
```

Allowed-by-default `pedantic` exceptions go in the same table with a one-line justification each — e.g. `module_name_repetitions = "allow"` (plane modules legitimately repeat their plane's name: `control::documents::Document`).

## 6. Module and visibility conventions

Recap from ARCHITECTURE Appendix A (ch07): `pub mod` planes at the crate root, private leaf modules, `pub(crate)`/`pub(super)` for internals, facade re-exports (`pub use models::{Document, Job};`) so the public surface stays flat. **C-STRUCT-PRIVATE** applies to every struct crossing a plane boundary — with the guideline's own exception: plain data records whose fields carry no invariant beyond construction (DTOs like `FetchedDoc`, `Fact`, `ScoredChunk`) may expose `pub` fields; anything whose fields must stay coherent (e.g. [`Config`](config.rs), the ID newtypes) keeps them private behind constructors and getters. `pipeline/` depends on traits and facades only — a PR that names a vendor type outside its owning plane is rejected in review, full stop.

## 7. Repository gates

The repository exposes the quality gates through the [Makefile](../Makefile). CI is not wired yet, so run these locally before every commit:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --no-deps      # docs build warning-free (C-LINK)
```

The equivalent one-shot command is `make verify`.

## 8. Review checklist (the 60-second version)

- [ ] `cargo fmt --check` clean; no hand-formatting fights
- [ ] New types: casing right, common traits derived, fields private, `Debug` non-empty
- [ ] New fallible API: `Result` + `# Errors` doc + `class()` where it's a port error; outcomes-as-values, not errors (§10)
- [ ] Boundary input parsed into newtypes; no `bool`/`Option` parameter flags
- [ ] Public items documented with `///` + example; prose hyperlinks
- [ ] Port changes: object-safe, `Send + Sync`, capabilities honest, contract tests updated in `tests/ports/`
- [ ] No vendor types outside their owning plane; no new dependency without a facade (§2)
