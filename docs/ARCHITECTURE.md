# ohara — System Architecture (v2.1)

> **Status:** design of record. v2.1 supersedes v2 and folds in the post-v2 architecture/data review: vector namespaces on the knowledge port, FTS5 schema, entity identity and merge protocol, typed aliases, fact-edge aggregation, queue ordering, deletion intent, and ops hardening. Appendix B lists the v1 → v2 and v2 → v2.1 change logs.

---

## 1. Overview

ohara is an embedded, zero-daemon, in-process pipeline: web scraping → clean-text transformation → semantic chunking → open-domain GraphRAG. It targets personal-scale corpora (10³–10⁶ chunks) on a single machine.

### 1.1 Three planes

| Plane | Implementation | Responsibility |
| :--- | :--- | :--- |
| **Control** | SQLite (WAL) | Document state machine, job queue, dedup hashes, BM25 index, audit trail |
| **Engine** | Fetch ladder (HTTP → impersonation → Obscura) | Network fetching, JS rendering, anti-bot handling |
| **Knowledge** | LadybugDB | HNSW vector collections + property graph with openCypher |

### 1.2 Design principles

1. **Embedded first.** One process plus the Obscura subprocess. ohara itself starts no services; the only optional external dependency is the user's own Ollama endpoint (§2).
2. **One owner per datastore.** `control/` owns SQLite, `knowledge/` owns LadybugDB, `engine/` owns the network. All schema and vendor knowledge lives in exactly one module.
3. **Ports at plane boundaries.** The pipeline depends only on traits. Contracts are behavioral (LSP), not just type-level.
4. **Identity discipline.** Content-derived hashes for *immutable derived rows* (`chunk_id`, `triplet_id`) make replay a no-op. Evolving *first-class entities* get stable surrogate IDs (uuidv7) and merge via an explicit protocol (§7.8) — a canonical name is data, not identity.
5. **Boundary failure is expected.** `Result` everywhere at boundaries (ch09-03); `panic!` only for broken internal invariants.
6. **Auditable.** Every stage transition and every error is recorded in `stage_events`.
7. **Reproducible.** Pinned models, pinned dependency versions, versioned migrations, `pipeline_version` on every document.
8. **Derived knowledge plane.** SQLite + `data/` are the system of record; the Ladybug knowledge plane is a rebuildable index (§7.9).

### 1.3 Module map and dependency direction

```
                 ┌──────────── pipeline.rs (worker loop) ────────────┐
                 │            pipeline/{scrape,clean,chunk,          │
                 │                      embed,extract,retrieve}      │
                 ▼                  ▼                ▼                ▼
          control.rs          engine.rs        knowledge.rs        llm.rs
          (SQLite)      ports▶(Fetcher)  ports▶(KnowledgeStore) ports▶(Llm)
                 │                  │                │
              SQLite          HTTP/Obscura      LadybugDB

   text.rs, config.rs: shared, dependency-free (text) / leaf (config)
```

`pipeline/` depends on plane **facades and traits only**. Planes never import `pipeline`. Port traits live with their consumer or their owner: `Fetcher` in `engine.rs`, `KnowledgeStore` in `knowledge.rs`, `Embedder` in `pipeline/embed.rs`, `Extractor` in `pipeline/clean.rs`, `QueryNormalizer` in `pipeline/retrieve.rs`, `Llm` in `llm.rs` — each re-exported through its plane facade.

---

## 2. Tech stack and dependency risk containment

| Component | Choice | Pinning / risk posture |
| :--- | :--- | :--- |
| Scraper | Obscura (Rust, native rendering, no Chromium) | **Very young project.** Behind `Fetcher` port; degradation ladder provides fallbacks; versioned JSON subprocess protocol; pinned binary version |
| Control store | SQLite via `rusqlite` (bundled), WAL | Stable; migrations versioned in `migrations/` |
| Cleaning | `readability` (Rust) + `html2md` | Behind `Extractor` port; swap = alternate impl |
| Text utils | `whatlang`, `symspell` | Pure functions in `text.rs`; symspell gets a domain dictionary |
| Knowledge store | LadybugDB (successor to Kùzu; embedded, columnar, openCypher) via the `lbug` crate (pinned 0.20.2) | **Young continuation of a wound-down project.** Behind `KnowledgeStore` port. **§2 build gates verified empirically against lbug 0.20.2 (2026-09-06):** (a) *transaction model* — MVCC snapshot reads run concurrently with the single write transaction; a second concurrent writer is refused (maps to `KnowledgeError::Unavailable`, Retry) — the single-worker default already serializes writers, so the actor-task contingency is not needed; (b) *edge-rewire/fold (§7.8)* — `CREATE`+`DELETE`+node deletion in one `BEGIN`/`COMMIT` verified. The bundled engine ships **no HNSW** (`query_hnsw_index` is absent); Phase 4 ships exact KNN via the in-engine `array_cosine_similarity` scalar — the HNSW index is a drop-in swap behind the same port (§11). Note: `lbug` links OpenSSL at link time (`libssl-dev` is a build prerequisite) |
| Embedder | BAAI `bge-small-en-v1.5` via ONNX (`fastembed` 6.0.2 + `ort`, fp32; tokenizer via `tokenizers`/`hf-hub` — rustls-only features, never native-tls) | **Pinned by name + version** (variant incl. quantization); recorded per chunk; see §4, §11.1. Model + tokenizer files fetched once from the HF hub into `data/models/` (fail-fast at boot), offline afterwards. Behind the `Embedder` port; the port also exposes `count_tokens` so chunk budgets are measured in the model's own tokenizer (§4) |
| Reranker | `bge-reranker-base` ONNX **int8** (Xenova export, ~280 MB) via fastembed's user-defined loader; FlashRank tiny models as a later swap; identity impl as baseline | Behind `Reranker` port; CPU-optimized models only — never fp32 (§11.1); model + tokenizer cached under `data/models` like the embedder |
| LLM | **Ollama (local, default)** — OpenAI-compatible endpoint, user-run; pinned on the reference profile: `phi4-mini:latest` on GPU (§11.2); cloud Haiku class **opt-in** | Behind `Llm` port; cost counters mandatory; endpoint health-checked at boot (config fail-fast) — mid-run unavailability maps to `LlmError::Unavailable` (Transient, backoff); §12 governs egress |

**Rule:** any dependency whose API is not its own standard (Obscura, LadybugDB, embedder runtime) must be reachable only through its port facade. No other module may name the vendor's types.

---

## 3. Data model overview

Two stores and disk, four kinds of data:

- **SQLite** — documents (registry, milestones, re-crawl scheduling), sites (per-host policies), jobs (per-stage execution), chunks + `chunks_fts` (registry of what was embedded, plus its BM25 index), entities + entity_aliases + entity_merges + er_review (canonical entity registry and its merge machinery), triplets (staged extraction output — the LLM cost cache), deletions (deletion intent), stage_events (audit), schema_migrations.
- **LadybugDB** — vector collections (chunk vectors per model namespace; entity-name vectors), `(:Chunk)-[:MENTIONS]->(:Entity)` edges, entity nodes, fact edges between entities.
- **Disk** — `data/raw/<doc_id>.html.gz`, `data/clean/<doc_id>.md`.

**ID taxonomy** (principle 1.2.4):

| Row | ID | Rationale |
| :--- | :--- | :--- |
| chunk | `sha256(doc_id:seq)` | immutable derived row; replay is a no-op |
| triplet | `sha256(chunk_id:subject:predicate:object)` | per-chunk evidence; cost cache |
| entity | uuidv7 surrogate | identity evolves (merges); looked up by `UNIQUE(canonical_name, entity_type)` |
| document, job | uuidv7 | time-ordered; claim ordering tiebreak |

The same logical row in both stores is always the same row, because every cross-store key is one of the above.

---

## 4. The embedding layer

- **Model (pinned):** `BAAI/bge-small-en-v1.5`, 384 dims, cosine distance, ONNX via a local runtime (e.g. `fastembed`-style). English-first — enforced at the Stage 2 gate (§8); a multilingual swap is a model migration, not a code change.
- **Tokenizer alignment:** chunk budgets are measured **in the embedder's tokenizer**, not characters or whitespace words. Budget: ≤ 512 tokens *including* the breadcrumb prefix.
- **Versioning as a namespace pointer:** one vector collection exists per model (`VectorSpace::Chunks { model_id }`, §9). `chunks.embedding_model` records, **per row**, the model whose vectors that row currently carries — it is the registry's pointer into a collection, and the boot sweep verifies each row against its own pointer (§7.3). Mixing models in one collection is structurally impossible.
- **Provider swap vs model swap:** the `Embedder` port makes the *provider* hot-swappable (ONNX ↔ API) at equal model. Changing the *model* is a migration: set `write_model = new` (dual-write), backfill chunk-by-chunk — write the new-collection vector, then update that row's `embedding_model` — then flip `read_model = new` (the atomic read switch), then drop the old collection. An interrupted backfill resumes cleanly because each row's pointer says which collection it belongs to. Never in-place.
- **Cost/latency:** local CPU ~5–15 ms per chunk; 15k chunks ≈ 2–4 minutes single-threaded — negligible next to LLM stages.
- **Quantization:** fp32 is the default; an int8 variant is an optional, golden-set-gated lever (§11.1). The variant is part of the `model_id` string and gets its own collection.

---

## 5. Control-plane schema (SQLite, v2.1)

**Connection pragmas** — set on *every* connection in `control/db.rs`: `foreign_keys = ON` (SQLite defaults it **off**; without it the CASCADEs below are inert), `journal_mode = WAL`, `synchronous = NORMAL`, `busy_timeout = 5000`.

**Timestamp rule:** every `TIMESTAMP` column stores SQLite's native UTC format (`YYYY-MM-DD HH:MM:SS`, as produced by `CURRENT_TIMESTAMP`); all application writes go through one `now()` helper in `db.rs`. Lexicographic order must equal chronological order — lease expiry and backoff comparisons depend on it.

```sql
CREATE TABLE IF NOT EXISTS schema_migrations (
    version    TEXT PRIMARY KEY,
    applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Per-host policies: Stage 1's hint table and §7.5's refresh policy live here.
CREATE TABLE IF NOT EXISTS sites (
    host             TEXT PRIMARY KEY,
    rate_limit_ms    INTEGER,        -- overrides the global token bucket
    recrawl_seconds  INTEGER,        -- default refresh interval; NULL = never
    fetch_hint       TEXT            -- 'plain' | 'impersonate' | 'browser'
);

CREATE TABLE IF NOT EXISTS documents (
    doc_id                TEXT PRIMARY KEY,          -- uuidv7 (time-ordered)
    source_url            TEXT NOT NULL,
    source_url_normalized TEXT NOT NULL UNIQUE,      -- URL-level dedup; normalization spec in §8 Stage 1
    raw_file_path         TEXT NOT NULL,
    clean_file_path       TEXT,
    clean_content_hash    TEXT UNIQUE,               -- content-level dedup
    status                TEXT NOT NULL CHECK(status IN
        ('NEW','SCRAPED','CLEANED','VECTORIZED','INDEXED',
         'FAILED_QUALITY','FAILED','ARCHIVED')),
    title                 TEXT,
    author                TEXT,
    language              TEXT,                      -- whatlang result; gated at Stage 2 (§8)
    word_count            INTEGER,
    token_count           INTEGER,
    chunk_count           INTEGER NOT NULL DEFAULT 0,
    http_status           INTEGER,
    etag                  TEXT,                      -- conditional re-crawl
    last_modified         TEXT,
    fetched_at            TIMESTAMP,
    next_crawl_at         TIMESTAMP,                 -- due date; NULL = never (§7.5)
    created_at            TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    last_processed_at     TIMESTAMP,                 -- set by the worker on each transition
    error                 TEXT,                      -- last failure detail
    pipeline_version      TEXT NOT NULL              -- reprocess when logic changes
);
CREATE INDEX IF NOT EXISTS idx_documents_status ON documents(status);
CREATE INDEX IF NOT EXISTS idx_documents_crawl  ON documents(next_crawl_at);

CREATE TABLE IF NOT EXISTS jobs (
    job_id           TEXT PRIMARY KEY,      -- uuidv7 (time-ordered claim tiebreak)
    doc_id           TEXT NOT NULL REFERENCES documents(doc_id) ON DELETE CASCADE,
    stage            TEXT NOT NULL CHECK(stage IN ('SCRAPE','CLEAN','VECTORIZE','EXTRACT')),
    status           TEXT NOT NULL CHECK(status IN ('PENDING','RUNNING','DEAD','DONE')),
    priority         INTEGER NOT NULL DEFAULT 5,   -- lower = sooner; assigned at enqueue
    attempts         INTEGER NOT NULL DEFAULT 0,
    max_attempts     INTEGER NOT NULL DEFAULT 5,
    next_attempt_at  TIMESTAMP,                     -- backoff gate (§6)
    lease_owner      TEXT,
    lease_expires_at TIMESTAMP,
    last_error       TEXT,
    params           TEXT,                          -- JSON stage params, e.g. {"embedding_model": "..."}
    created_at       TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at       TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(doc_id, stage)                       -- enforces the §6 one-row invariant
);
CREATE INDEX IF NOT EXISTS idx_jobs_claim ON jobs(stage, status);

CREATE TABLE IF NOT EXISTS chunks (
    id              INTEGER PRIMARY KEY,  -- internal surrogate: FTS5 external-content key; never leaves SQLite
    chunk_id        TEXT NOT NULL UNIQUE, -- sha256(doc_id || ':' || seq) — the cross-store identity
    doc_id          TEXT NOT NULL REFERENCES documents(doc_id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    header_path     TEXT,                 -- "Title > H1 > H2"
    text            TEXT NOT NULL,        -- display text
    embed_text      TEXT NOT NULL,        -- exact string embedded (with breadcrumb)
    token_count     INTEGER NOT NULL,
    embedding_model TEXT NOT NULL,        -- model whose vectors this row currently carries (§4)
    content_hash    TEXT NOT NULL,        -- chunk-level dedup
    UNIQUE(doc_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_chunks_doc  ON chunks(doc_id);
CREATE INDEX IF NOT EXISTS idx_chunks_hash ON chunks(content_hash);

-- External-content BM25 index over chunks, transactionally trigger-synced.
-- Recovery: INSERT INTO chunks_fts(chunks_fts) VALUES('rebuild');  (§7.9)
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    text,
    content = 'chunks',
    content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER IF NOT EXISTS chunks_fts_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER IF NOT EXISTS chunks_fts_ad AFTER DELETE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER IF NOT EXISTS chunks_fts_au AFTER UPDATE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
    INSERT INTO chunks_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE TABLE IF NOT EXISTS entities (
    entity_id      TEXT PRIMARY KEY,      -- uuidv7 surrogate — stable for life (§7.8)
    canonical_name TEXT NOT NULL,
    entity_type    TEXT NOT NULL CHECK(entity_type IN
        ('PERSON','ORGANIZATION','LOCATION','EVENT','CONCEPT','PRODUCT')),
    subtype        TEXT,                  -- free-form refinement under the supertype
    created_at     TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(canonical_name, entity_type)   -- the lookup and MERGE key
);

CREATE TABLE IF NOT EXISTS entity_aliases (
    alias       TEXT NOT NULL,            -- normalized surface form
    entity_type TEXT NOT NULL,            -- homographs live per type: Jordan/PERSON ≠ Jordan/LOCATION
    entity_id   TEXT NOT NULL REFERENCES entities(entity_id),
    source      TEXT,                     -- 'extraction' | 'user' | 'merge'
    created_at  TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (alias, entity_type)
);

CREATE TABLE IF NOT EXISTS entity_merges (
    loser_id  TEXT NOT NULL REFERENCES entities(entity_id),
    winner_id TEXT NOT NULL REFERENCES entities(entity_id),
    reason    TEXT,
    merged_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (loser_id)                -- an entity folds away exactly once
);

CREATE TABLE IF NOT EXISTS er_review (    -- cross-doc merge candidates awaiting a decision
    id         INTEGER PRIMARY KEY,
    entity_a   TEXT NOT NULL,             -- entity ids at detection time; stale rows resolve
    entity_b   TEXT NOT NULL,             --   transitively through entity_merges (§7.8)
    score      REAL,
    status     TEXT NOT NULL CHECK(status IN ('PENDING','MERGED','REJECTED')),
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS triplets (
    triplet_id   TEXT PRIMARY KEY,        -- sha256(chunk_id || subject || predicate || object)
    chunk_id     TEXT NOT NULL REFERENCES chunks(chunk_id) ON DELETE CASCADE,
    subject      TEXT NOT NULL,           -- surface strings: evidence, not identity (§8 Stage 4)
    subject_type TEXT NOT NULL,
    predicate    TEXT NOT NULL CHECK(predicate IN
        ('LOCATED_IN','PART_OF','CREATED_BY','CAUSED','AFFECTED','PARTICIPATED_IN',
         'ASSOCIATED_WITH','PRODUCES','FOUNDED','DEPENDS_ON')),
    object       TEXT NOT NULL,
    object_type  TEXT NOT NULL,
    properties   TEXT,                    -- JSON: occurred_on, as_of, …
    model        TEXT NOT NULL,           -- extractor model/prompt version
    extracted_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_triplets_chunk ON triplets(chunk_id);

CREATE TABLE IF NOT EXISTS deletions (    -- deletion intent; drives §7.6 and the boot sweep
    doc_id       TEXT PRIMARY KEY REFERENCES documents(doc_id) ON DELETE CASCADE,
    requested_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    reason       TEXT
);

CREATE TABLE IF NOT EXISTS stage_events (
    event_id INTEGER PRIMARY KEY,
    doc_id   TEXT,
    job_id   TEXT,
    stage    TEXT,
    outcome  TEXT,                        -- DONE | RETRY | DEAD | PANIC
    detail   TEXT,                        -- error chain / decision reason
    ts       TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_stage_events_doc ON stage_events(doc_id, ts);
```

Notes:

- Chunk and triplet writes use `ON CONFLICT(...) DO UPDATE`, **never** `INSERT OR REPLACE` — REPLACE deletes and re-inserts the row, churning the `chunks.id` surrogate and the FTS rowid mapping.
- `documents.status` starts at `NEW` (enqueued, no milestone completed yet) — the CHECK needs a birth state because jobs, not the document, carry execution state.
- `UNIQUE(doc_id, stage)` turns §6's "one job row per (doc_id, stage)" from a convention into an enforced invariant. Re-enqueueing a stage (chaining into a replayed document, re-crawl) therefore means `INSERT … ON CONFLICT DO UPDATE` — *resurrect* a terminal (`DEAD`/`DONE`) row to `PENDING` with `attempts = 0`; live `PENDING`/`RUNNING` rows are never disturbed.
- `stage_events` deliberately has **no FK** — the audit trail outlives deleted rows; the maintenance tick prunes it at 90 days (config).
- `triplets` rows are per-chunk evidence keyed by surface strings. Entity-level facts live in Ladybug (§8 Stage 4); the `triplets` table is the extraction checkpoint and cost cache, nothing more.
- `documents` milestone note: the doc stays at its last *completed* milestone; execution state lives in `jobs` (v1's `VECTORIZING` is gone).

---

## 6. Job state machine and queue semantics

**One job row per (doc_id, stage).** Document status marks *completed milestones*; jobs carry execution state. Priorities are assigned at enqueue (CLI/config; default 5). SCRAPE jobs are created by `ohara enqueue <url>` and by the re-crawl maintenance tick.

```
PENDING ──claim──► RUNNING ──ok──► DONE (+ insert next stage's job, same tx)
   ▲                 │ │
   │  transient err  │ └── fatal ──► drain workers → shutdown → reconcile at next boot
   └──────◄──────────┘
     attempts++, next_attempt_at = now + jittered backoff

RUNNING with expired lease ──► reclaimable (attempts++, last_error = 'lease expired')
PENDING/RUNNING with attempts ≥ max_attempts, or permanent error ──► DEAD
```

- **Stage chaining:** on `DONE`, the worker inserts the next stage's `PENDING` job *in the same transaction* as the milestone update (SCRAPE→CLEAN→VECTORIZE→EXTRACT) — a crash between milestone and chaining is impossible. Chained jobs inherit the completing job's priority; if the successor row already exists (replayed document), the insert resurrects it per §5's `UNIQUE(doc_id, stage)` note. With `graph_enabled = false` the chain ends at VECTORIZE — documents stay `VECTORIZED` and remain retrievable via BM25 + vector. An EXTRACT `DONE` sets `INDEXED`; there is no next stage.
- **Atomic claim** (SQLite ≥ 3.35, WAL, pragmas per §5):

```sql
UPDATE jobs
   SET status = 'RUNNING', lease_owner = :worker,
       lease_expires_at = :now + 60 seconds,
       attempts   = attempts + (CASE WHEN status = 'RUNNING' THEN 1 ELSE 0 END),
       last_error = CASE WHEN status = 'RUNNING' THEN 'lease expired'
                         ELSE last_error END
 WHERE job_id = (
     SELECT job_id FROM jobs
      WHERE stage = :stage
        AND ( (status = 'PENDING'
               AND (next_attempt_at IS NULL OR next_attempt_at <= :now))
           OR (status = 'RUNNING' AND lease_expires_at < :now) )
      ORDER BY priority, created_at, job_id
      LIMIT 1)
RETURNING job_id, doc_id, attempts, params;
```

- **Lease heartbeat** while a stage runs; expired leases are reclaimable (crash-safe).
- **`attempts` counts ended executions, not claims.** It increments exactly once per ended run: on failure recording, or on expired-lease reclaim (with `last_error = 'lease expired'`). A clean crash mid-lease is therefore punished exactly once, when the lease expires — not twice.
- **Classification-driven retry:** port errors carry a class (`Retry` / `Permanent` / `Fatal`, §10). `Retry` → `attempts++`, `next_attempt_at = now + jittered backoff`, back to `PENDING` (the `next_attempt_at` predicate above makes backoff real — without it a `PENDING` retry would be immediately reclaimable). `Permanent`, or attempts exhausted → `DEAD`, and the document is set to `FAILED` with `error = last_error`. `Fatal` → stop scheduling, drain, exit non-zero; the boot sweep (§7.3) finishes recovery.
- **Terminal mapping and recovery:** `DEAD` job ⇒ `documents.status = 'FAILED'`. Recovery is `ohara requeue --doc <id>` (resets that doc's jobs to `PENDING`, `attempts = 0`), or automatic when `pipeline_version` bumps past a failed doc.
- **`ARCHIVED`:** user-marked state (`ohara archive <doc>`). Excluded from re-crawl scheduling; chunks remain queryable. It is a retention decision, not an execution state.
- **Stage 4 progress** is *triplets coverage* (chunks of the doc that already have triplets, §7.7), not per-chunk jobs — there is exactly one EXTRACT job per document. The document goes `INDEXED` when its EXTRACT job is `DONE`.
- **Concurrency:** single worker loop by default (honest for an embedded tool); the lease protocol makes multi-worker safe when needed. SQLite is the single writer; WAL gives concurrent readers.

---

## 7. Cross-store consistency protocol

SQLite and LadybugDB have **no shared transaction**. The protocol makes every crash window recoverable:

1. **Identity discipline (§3).** Chunks and triplets are content-hash keyed — replaying any stage is a no-op on already-written data. Entities are uuidv7-keyed and found by `UNIQUE(canonical_name, entity_type)`; Stage 4's Cypher `MERGE` matches on that key, which is what makes replay idempotent for entities too.
2. **Intent before write.** The worker sets the job to `RUNNING` *before* touching LadybugDB; the document milestone advances only after the knowledge-plane write returns `Ok`.
3. **Boot reconciliation sweep.** On startup, jobs found `RUNNING` with expired leases are re-verified: does the chunk have a vector in the collection its row's `embedding_model` points at (`has_vector`)? Then the job resumes idempotently — `:MENTIONS` and fact edges are simply re-merged (idempotent), never "checked then written."
4. **Re-chunk policy.** Re-chunking a doc deletes all its chunks (`delete_doc`: every vector collection + graph edges) before re-inserting. HNSW deletes may be tombstones; the sweep tracks the tombstone ratio and triggers a rebuild above a threshold (e.g. 25%) — using the §7.9 machinery.
5. **Re-crawl.** The maintenance tick enqueues SCRAPE for documents with `next_crawl_at <= now` (excludes `FAILED*`/`ARCHIVED`). Conditional GET via `etag`/`last_modified`; a 304 bumps `fetched_at` and doubles the interval (from `sites.recrawl_seconds`, capped) — classic adaptive refresh; changed content resets it. A changed doc replays: re-clean → re-chunk (delete-first) → re-extract, and its **milestone rewinds to the last still-true stage** (`CLEANED`), with jobs re-enqueued per stage. `clean_content_hash` equality short-circuits before any of that.
6. **Delete flow.** `ohara delete <doc>` inserts a `deletions` intent row **first**. The worker (or boot sweep) then: `delete_doc` in Ladybug (all vector collections + graph) → one SQLite transaction deletes the `documents` row (CASCADE removes jobs, chunks, triplets, and the intent row). Because the intent row exists exactly while the document row does, an interrupted deletion is re-executed idempotently at boot. Entities whose `MENTIONS` degree drops to zero become GC candidates after a grace period (config) — another doc may still be re-crawling.
7. **`triplets` as the extraction checkpoint.** Triplet extraction is the expensive stage and lands in SQLite first (§8, Stage 4); a resume skips chunks that already have triplets. LLM output is never paid for twice. (A *content change* legitimately invalidates the cache — the re-chunk cascade removes stale triplets with their chunks, §5.)
8. **Entity merge protocol** (offline; executed by `ohara er merge`, never on the hot path):
   1. Pick the winner: higher `:MENTIONS` degree, else older.
   2. SQLite transaction: remap the loser's aliases to the winner (`source = 'merge'`), insert the `entity_merges` audit row, close out any `er_review` rows referencing the pair.
   3. Ladybug fold: rewire the loser's `:MENTIONS` edges and fact edges to the winner (property-conflicting occurrences are unioned per §8 Stage 4), delete the loser node.
   4. `triplets` are untouched — they are surface-string evidence, not identity. Stale `er_review` candidates resolve transitively through `entity_merges`.
9. **Derived-data principle.** SQLite + `data/` are the system of record; Ladybug is a rebuildable index. Full rebuild = re-embed every chunk from `chunks.embed_text` into the current model collection, then re-merge entities/facts from `triplets` via the alias/type mapping. The same machinery powers the §4 model migration. SQLite-side derived indexes rebuild natively (`chunks_fts` `'rebuild'` command). Cost: re-embedding 10⁶ chunks is CPU-hours — an acceptable disaster-recovery path, and the reason `embed_text` is stored exactly as embedded.

---

## 8. Stage specifications

### Stage 1 — Scrape

- **Fetcher ladder** (selection per request, escalating on signals): plain HTTP (fast, cheap) → impersonated client → Obscura (JS + stealth). Escalation triggers: `FetchError::AntiBot`, JS-shell heuristic (tiny content + script-heavy raw HTML), or `sites.fetch_hint`.
- **URL normalization** (load-bearing for `source_url_normalized` dedup — specified, not folklore): lowercase scheme/host, punycode IDN, drop default ports and fragments, sort query parameters, strip configurable tracking params (`utm_*`, `fbclid`, `gclid`, …), absolutize relative URLs against `final_url`. Implemented once in the `NormalizedUrl` newtype with fixture tests.
- `FetchedDoc { html, js_executed, final_url, status, content_type, etag, last_modified, fetched_at }` — the contract is "return what you fetched, labeled" (§9), and the pipeline escalates when the label says rendering didn't happen. The validators ride along for §7.5 conditional re-crawl.
- **Per-fetch policy:** the port takes `fetch_with_policy(url, &FetchPolicy)` — the stage reads the control plane (`sites.rate_limit_ms`, the robots toggle) and the fetcher enforces (§8 politeness floor is `max(impl default, policy)`); plain `fetch(url)` applies the default. Robots rules use a per-host cache; an unreadable robots.txt is cached as disallow-all (conservative RFC 9309).
- Payload → `data/raw/<doc_id>.html.gz`; document row → `SCRAPED`.
- **Politeness:** `robots.txt` honored (config toggle, default on; cached per host); per-domain token bucket (`sites.rate_limit_ms` overrides the global default 1 req / 2 s); global concurrency cap; honest User-Agent.

### Stage 2 — Clean

1. `Extractor` port: boilerplate removal → primary-content HTML → Markdown (structure preserved: headings, lists, tables).
2. Sanitize: resolve relative URLs, strip inline base64/SVG, Unicode NFC, collapse whitespace.
3. **Quality gate is a value, not an error:** `CleanOutcome::Accepted | Rejected(reason)`. Rejections → `FAILED_QUALITY` with the reason recorded: word count < 50, paywall markers, boilerplate-only, **and language outside `target_languages` (default `["en"]`)** — the embedder is English-only and the symspell dictionary is English; embedding non-English text would poison the vector space. Duplicates (by `clean_content_hash`) → `Ok(SkippedDuplicate)`; `Err` is reserved for "the operation couldn't do its job" (§10).
4. Optional LLM enrichment (summary, taxonomy tags): cloud opt-in per §12.

### Stage 3 — Chunk & vectorize

- **Layer 1:** split on `#`/`##`/`###` boundaries. **Layer 2:** sections over budget → recursive split (`\n\n` → `\n` → sentence) with 10–15% overlap. Tables and fenced code blocks are atomic — never split mid-block; a table larger than the budget becomes its own chunk.
- **Budget:** ≤ 512 tokens in the *embedder's tokenizer*, breadcrumb included.
- **Breadcrumb prefix** (part of `embed_text`, kept separate in `text`): document title + header path; doc summary once Stage 2 enrichment is on.
- Chunk-level exact-dup skip via `chunks.content_hash` (identical `embed_text`s share one inference; every chunk id still gets its vector).
- Batch embed → chunk rows upserted first (deterministic `chunk_id` = `sha256(doc_id:seq)`, `embedding_model` = the model, `ON CONFLICT(doc_id, seq) DO UPDATE` — the FTS triggers fire in the same transaction), then `upsert_vectors(VectorSpace::Chunks { model }, doc_id, …)`. Replay semantics per §7.3: identical signature → repair missing vectors only; drift → §7.4 delete-first. → `VECTORIZED`.

### Stage 4 — Extract graph

- **Two-layer model:** `triplets` (SQLite) are **per-chunk evidence** — surface strings, checkpoint, cost cache. **Fact edges** (Ladybug) are the **entity-level aggregation**. Confusing the two is how graphs become hairballs.
- **Triplet extraction:** LLM per chunk → JSON triplets → validated against the predicate/type-compatibility matrix → staged in `triplets` → mapped through the alias/type tables → merged into Ladybug.
- **Ontology:**
  - Supertypes: `PERSON, ORGANIZATION, LOCATION, EVENT, CONCEPT, PRODUCT`. **Date/Time is a property** (`occurred_on` on an edge), not a node type.
  - `CONCEPT` is bounded: extracted only for noun-phrase arguments passing a heuristic; free-form `subtype` refines it.
  - **Type-compatibility matrix** (subject_type, predicate, object_type) enforced in the extraction prompt *and* re-validated post-extraction; violations are logged to `stage_events` and dropped.
- **Fact-edge identity: `(subject_id, predicate, object_id)`.** Multiple chunks asserting the same fact MERGE into one edge carrying `support_count`, a capped `evidence` list of chunk ids, and `occurrences` (capped JSON array of `occurred_on` values; `as_of` facts keep the latest). Surface-form duplicates ("Obama" vs "Barack Obama") collapse naturally through the entity mapping. Conflicting-object cases (`LOCATED_IN` two places) are different edges by construction — flagged for review, not merged.
- **Entity resolution — conservative, type-consistent, incremental:**
  1. **Typed alias hit:** exact `(normalized alias, entity_type)` match → existing `entity_id`. A same-alias-different-type hit is a **miss**, not a resolution (Jordan/PERSON ≠ Jordan/LOCATION).
  2. Else candidate match *within the same supertype*: normalized-name similarity ≥ threshold OR entity-name embedding similarity ≥ threshold (the `EntityNames` collection, §9). Never across supertypes.
  3. Per-document union-find resolves transitive merges at write time. Cross-document merge candidates go to `er_review`; merges execute offline via the §7.8 protocol. Ambiguous cases stay separate and are flagged in `stage_events`.
- **Cross-linking:** `(:Chunk)-[:MENTIONS]->(:Entity)` — `MERGE` on `chunk_id`/`entity_id` makes relinking idempotent.
- Document → `INDEXED` when its EXTRACT job is `DONE` (§6); per-chunk progress = triplets coverage.

### Stage 5 — Retrieval (GraphRAG)

*Baseline form landed (§15 step 5): steps 1 (language detect only), 3 (BM25 + vector paths), 4 (RRF), 5 (rerank with degradation), and 7 (the golden-set harness). The graph path, symspell, HyDE, and Llm synthesis land with steps 6–8. Measured baseline on the golden set: both paths recall@20 = 1.000, fused MRR = 1.000.*

1. **Query preprocessing:** language detect, symspell with a domain dictionary (it "corrects" jargon otherwise), optional HyDE (flag; +300–500 ms). *Landed: whatlang detection is confidence-gated at 0.5 — measured, short technical queries sit at 0.02–0.10 confidence and mislabel; below the floor the query is treated as English (the Stage 2 gate bounds corpus languages anyway).*
2. **Query entities:** typed alias match against `entity_aliases` — a homograph alias returns **all** its type-variants and lets rerank/graph context disambiguate — plus embedding KNN over the `EntityNames` collection above a threshold.
3. **Three paths:** BM25 via `chunks_fts` (embeddings are weak on exact identifiers like "SQLite"):

   ```sql
   SELECT c.chunk_id, c.text
     FROM chunks_fts f JOIN chunks c ON c.id = f.rowid
    WHERE chunks_fts MATCH :q
    ORDER BY bm25(chunks_fts) LIMIT 50;
   ```

   vector KNN over `VectorSpace::Chunks { read_model }` (`k=50`); graph path (`facts_within_hops` + `chunks_for_entities` via `:MENTIONS`).
4. **Fusion:** Reciprocal Rank Fusion (k=60) across the three lists → top-50 candidates.
5. **Rerank:** `Reranker` port over the 50-candidate pool → top-5. Reranking is **fallible and degrades, never fails the query**: on `RerankError` the fusion order is returned as-is and the failure is counted. FlashRank tiny models for local latency; identity reranker as benchmarking baseline and fallback.
6. **Synthesis:** `Llm` port; context = top chunks + graph facts rendered as a labeled fact list; citations = `chunk_id`s; synthesized answer cites inline.
7. **Evaluation:** 50-query golden set in `tests/` fixtures; metrics: recall@20 per path, MRR of the fused list, rerank delta. The vector-path recall@20 is also the acceptance gate for the HNSW knobs (§11). Run before/after any retrieval change — "high precision" is a measured claim, not an aspiration.

---

## 9. Ports & substitution (LSP)

The trait keyword gives *signature* substitutability; LSP requires *behavioral* substitutability, which the compiler cannot check. Three rules make it real:

1. **Contracts as postconditions, not mechanisms.** E.g. "after `delete_doc(id)`, `knn` in *any* collection never returns that doc's ids" — every backend honors it differently (tombstone, rebuild, filter-out), so every backend substitutes.
2. **Declare capabilities honestly.** Plain HTTP cannot execute JS; pretending otherwise violates LSP by lying in the contract. Capability structs let the caller compose and escalate (the fetch ladder).
3. **No vendor language through a port.** Never `fn query(&self, cypher: &str)` — only Cypher-speaking impls qualify, making substitution an illusion.

| Port | Hides | Key contract (postconditions) | Swap candidates |
| :--- | :--- | :--- | :--- |
| `Fetcher` (`engine.rs`) | Obscura | Rendered-or-labeled HTML; unified `FetchError` taxonomy; `capabilities()` | HTTP ↔ impersonation ↔ chromiumoxide ↔ Obscura |
| `KnowledgeStore` (`knowledge.rs`) | LadybugDB | All vector ops are `VectorSpace`-scoped; deterministic upserts; delete→KNN postcondition across every collection; filtered KNN results satisfy the filter (impl may over-fetch) | LadybugDB ↔ Kùzu ↔ vector-lib + SQLite edges |
| `Embedder` (`pipeline/embed.rs`) | ONNX runtime | Order-preserving batch; `model_id()`/`dim()` are instance state; provider swap ≠ model swap | fastembed ↔ raw ONNX ↔ API (equal model) |
| `Reranker` (`pipeline/retrieve.rs`) | FlashRank | Returns all candidates sorted by relevance, descending; **fallible** | FlashRank ↔ ONNX bge ↔ API ↔ identity |
| `Llm` (`llm.rs`) | provider SDKs | Completion + schema-validated JSON (schema-constrained where the provider supports it, e.g. Ollama structured outputs); temperature-0 determinism for extraction; cost counters | Ollama (default) ↔ Haiku (opt-in) ↔ other OpenAI-compatible endpoints |
| `Extractor` (`pipeline/clean.rs`) | readability/html2md | HTML → (title, byline, markdown); pure; URLs absolutized; scripts stripped | alternate extractors |
| `QueryNormalizer` (`pipeline/retrieve.rs`) | whatlang/symspell | Pure; `normalize(q) -> (lang, q')` | lingua, cld, HyDE-as-strategy |
| `ControlStore` (`control.rs`) | SQLite | Queue semantics: atomic claim, lease, at-least-once | SQLite ↔ Postgres (later). *One impl ships; the trait is a cheap seam, module isolation does most of the work* |

Sketch (note: `async fn` in traits is not dyn-compatible — ports use `#[async_trait]`/boxed futures and stay object-safe; no generic methods on `dyn` ports):

```rust
/// Which vector collection a call addresses. One per model (§4) + entity names.
pub enum VectorSpace {
    Chunks { model_id: ModelId },
    EntityNames,
}

#[async_trait]
pub trait Fetcher: Send + Sync {
    fn capabilities(&self) -> FetchCapabilities;            // { js_rendering, stealth }
    async fn fetch_with_policy(&self, url: &NormalizedUrl, policy: &FetchPolicy)
        -> Result<FetchedDoc, FetchError>;                  // §8 politeness + robots
    async fn fetch(&self, url: &NormalizedUrl) -> Result<FetchedDoc, FetchError> { /* default policy */ }
}

#[async_trait]
pub trait KnowledgeStore: Send + Sync {
    fn capabilities(&self) -> KsCapabilities;               // { filtered_ann, graph_traversal }
    // vector ops — collection-scoped; doc_id records membership so
    // delete_doc can honor the delete→KNN postcondition (§7.6)
    async fn upsert_vectors(&self, space: VectorSpace, doc_id: &str, ids: &[&str],
                            vectors: &[Vec<f32>]) -> Result<(), KnowledgeError>;
    async fn knn(&self, space: VectorSpace, q: &[f32], k: usize,
                 f: &ChunkFilter) -> Result<Vec<ScoredHit>, KnowledgeError>;
    async fn has_vector(&self, space: VectorSpace, id: &str) -> Result<bool, KnowledgeError>;
    // graph ops
    async fn upsert_entity(&self, e: &EntityRecord) -> Result<(), KnowledgeError>;
    async fn link_mention(&self, chunk_id: &str, entity_id: &str) -> Result<(), KnowledgeError>;
    async fn fold_entity(&self, loser: &str, winner: &str) -> Result<(), KnowledgeError>;  // §7.8
    async fn delete_doc(&self, doc_id: &str) -> Result<(), KnowledgeError>; // every collection + graph
    async fn chunks_for_entities(&self, ids: &[&str]) -> Result<Vec<String>, KnowledgeError>;
    async fn facts_within_hops(&self, ids: &[&str], hops: u8) -> Result<Vec<Fact>, KnowledgeError>;
}

pub trait Embedder: Send + Sync {
    fn model_id(&self) -> &str;                             // "bge-small-en-v1.5"
    fn dim(&self) -> usize;                                 // 384
    fn count_tokens(&self, text: &str) -> usize;            // §4: budgets measured in
                                                            // the model's tokenizer
    // Sync by contract: CPU-bound batch. Callers invoke it inside spawn_blocking.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

#[async_trait]
pub trait Reranker: Send + Sync {
    fn name(&self) -> &str;
    // Fallible: RerankError { ModelUnavailable, Inference(String) }.
    // Callers degrade to fusion order on Err (§8 Stage 5) — never fail the query.
    async fn rerank(&self, query: &str, candidates: Vec<ScoredChunk>)
        -> Result<Vec<ScoredChunk>, RerankError>;
}
```

**Verification — contract tests.** One golden suite per port in `tests/ports/<port>/`, run against *every* impl *including fakes* (in-memory `KnowledgeStore`, identity `Reranker`, canned-HTML `Fetcher`). Suites include the **error-mapping tests** (each impl must collapse native failures into the port's taxonomy) and the **collection-isolation test** (a vector written to `Chunks{m1}` is invisible to `Chunks{m2}` and `EntityNames`). LSP becomes a CI-checked property, not an aspiration.

---

## 10. Error handling (Rust Book ch09, applied)

**Policy:** `Result` at every boundary — boundary failure is expected (ch09-03 names ohara's exact cases: malformed data, rate limits). `panic!` only for broken internal invariants. **Domain outcomes are values, not errors** (paywalled/duplicate/low-quality/wrong-language → `CleanOutcome`, so `?` never routes a normal outcome into the retry machinery).

**Layers:**

```rust
// Port layer — taxonomy + retry class; every impl maps native errors into it (thiserror derives From).
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("anti-bot block on {url}")]  AntiBot  { url: String },
    #[error("timeout after {secs}s")]    Timeout  { secs: u64 },
    #[error("not found: {url}")]         NotFound { url: String },
    #[error("protocol violation: {0}")]  Protocol(String),
}

// Stage layer — the only code that decides what this error means for this job.
pub enum StageError {
    Transient { source: Box<dyn Error + Send + Sync>, class: Class, attempt: u32 }, // → backoff → PENDING
    Permanent { reason: String },                                                   // → DEAD
    Fatal    { source: Box<dyn Error + Send + Sync> },                              // → drain + reconcile
}
```

**Retry-class mapping** (part of every port's contract; contract tests assert it):

| `FetchError` variant | Class | Rationale |
| :--- | :--- | :--- |
| `AntiBot` | Retry | fetch ladder escalates first; blind retries are throttled |
| `Timeout` | Retry | transient network condition |
| `NotFound` | Permanent | URL is gone; retrying cannot succeed |
| `Protocol` | Retry, then Permanent after N | may be a version mismatch or a fluke; `N` small |

- **Propagation with context:** `?` + `From` conversions (thiserror) move errors up; the worker writes every `Err` (full `source()` chain) to `stage_events` inside a tracing span carrying `doc_id`/`stage`/`job_id`. Nothing is swallowed silently.
- **Panic policy:**

| Situation | Choice |
| :--- | :--- |
| Malformed page, 403, rate limit, bad LLM JSON | `Result` |
| Obscura exits with unknown code | `Result` → `FetchError::Protocol` (external behavior is data) |
| Store returns corrupt data | `Result` → `StageError::Fatal` → drain + reconcile (audit-preserving shutdown beats panic) |
| Broken internal invariant (ID collision, post-migration schema mismatch) | `panic!` / `expect("…invariant…")` |
| Validated-at-boot config assumption | `expect` with the invariant stated in the message — never bare `unwrap` |
| Tests | `unwrap`/`expect` freely |

- **Worker isolation:** each stage run is wrapped in `catch_unwind`; a panicking job is recorded (`outcome = PANIC`) and the loop continues. `panic = "unwind"` stays in release profiles (the default) — `abort` would let one poisoned document kill the process and forfeit isolation. Panics bypass normal audit/reconciliation, which is exactly why they must remain rare bugs.
- **Validation as types (ch09-03 `Guess` pattern):** boundary newtypes — `NormalizedUrl` (§8 Stage 1 spec), `DocumentId`, `ChunkId`, `CanonicalName`, `TokenBudget` — parse once at the edge, then invalid states are unrepresentable downstream. Ports take validated types.
- **Housekeeping:** `thiserror` in the library, `anyhow` in the binary (`main() -> Result<()>` → non-zero exit); `#![deny(clippy::unwrap_used)]` outside tests; `RUST_BACKTRACE` documented for panic triage.

---

## 11. Performance and cost model

The performance story is **LLM-dominated**; everything else is rounding error. Illustrative figures (order-of-magnitude, per 1,000 documents at ~15 chunks/doc):

| Stage | Volume | Local cost | Cloud cost (Haiku-class, illustrative pricing) |
| :--- | :--- | :--- | :--- |
| Scrape + clean | 1k docs | minutes (network-bound) | — |
| Embed | 15k chunks | ~2–4 min CPU | $0 (local) |
| Summaries | 1k calls | — | ~$0.5–1 |
| **Triplet extraction** | **15k calls** (~700 in / 150 out tok) | Ollama `phi4-mini`: **GPU ≈ 4–7 h**; all-CPU fallback ≈ 1 day with parallelism (§11.2) | **~$5–8** |
| Entity resolution | in-process | ms-scale | — |
| Retrieval (per query) | 3 paths + rerank | ~50–300 ms | synthesis only |

- Rerank realism: bge-reranker on CPU over 50 × 400-token pairs is 100–500 ms — hence FlashRank tiny models as the default.
- Levers: extraction batch size, per-stage concurrency, local Ollama extraction (free, no data egress — wall-clock bound; parallel requests/batched prompts help), prompt versioning to avoid re-paying extraction after prompt tweaks (`triplets.model` guards this). Pin the chosen Ollama model by name + version like any other model (§1.2.7); a model change is an extraction migration, not a config flip.
- **HNSW params:** `M=16`, `ef_construction=64`, and `ef_search = max(128, 2·k)` — 128 at the k=50 rerank pool (an `ef` below `k` cannot return `k` candidates with sane recall). The knobs are gated by vector-path recall@20 on the golden set (§8.5), not by folklore; revisit at >1M chunks. **Current posture:** the bundled `lbug` engine ships no HNSW, so retrieval runs *exact* KNN in-engine (`array_cosine_similarity`, order-limited) — correct, sub-10ms at personal-corpus scale, and the parameterized index becomes a drop-in port impl swap when it ships.

### 11.1 Quantization policy

- **Embedder: fp32 by default.** Its vectors feed retrieval *and* ER name matching, so quantization loss is systemic risk; the CPU cost is already rounding error (~130 MB, 5–15 ms/chunk). int8 dynamic quantization is an optional lever for low-power targets (2–4× CPU speedup), **gated by vector-path recall@20 on the golden set** (§8.5).
- **Reranker: int8 by default.** Query-time only — nothing stored to contaminate, and the degradation policy (§8.5) caps the risk. FlashRank's bundled models ship CPU-optimized (int8) ONNX — the default path gets quantization for free. The optional bge-reranker-base path uses an int8 ONNX export (~280 MB, 30–150 ms/query on the reference CPU, gated by the rerank-delta metric). Never run it fp32 (~1.1 GB).
- **Variant is part of model identity.** Quantization goes into the `model_id` string (`bge-small-en-v1.5` vs `bge-small-en-v1.5:int8`) and is recorded per row in `chunks.embedding_model`; one collection per variant (§4) makes mixing structurally impossible.
- **fp16 is out of scope:** it's a GPU-only win on most CPUs, and no CUDA execution path ships — CPU int8 is the sweet spot for an embedded tool.

### 11.2 Reference hardware envelope

A concrete instance of the cost model (the development machine) — the knobs move with hardware, the shape doesn't:

- **Device:** i5-13420H (8C/12T, AVX2 + AVX_VNNI, no AVX-512/AMX), 15 GB RAM, RTX 4050 Mobile 6 GB VRAM (proprietary 595 driver, Ubuntu prebuilt signed kernel modules — no DKMS), ~565 GB free disk, Ollama local.
- **Accelerator split — LLM on GPU, everything else on CPU.** The GPU belongs to the LLM (Ollama) and to nothing else: ohara itself contains no GPU code — knowledge plane, embedder, and reranker stay CPU/int8, because a CUDA EP adds hundreds of MB of dependency weight to an embedded tool for wins the cost model doesn't need. The all-CPU fallback profile remains valid for driver-less machines: extraction ≈ 1 day per 15k chunks with parallelism.
- **Pinned on this profile:** embedder `bge-small-en-v1.5` fp32 in-process (~130 MB; 15k chunks ≈ 2–4 min on 12 threads); reranker FlashRank tiny default / bge-reranker-base **int8** (~280 MB, VNNI-accelerated); extraction LLM **`phi4-mini:latest`** via Ollama on GPU — fits 6 GB VRAM with headroom, tolerates `OLLAMA_NUM_PARALLEL = 2–4`, 15k calls ≈ **4–7 h**. Quality fallback `llama3.1:8b-instruct-q4_K_M` (~4.9 GB VRAM, ~4k context, parallel ≤ 2) ≈ 8–12 h. Cloud Haiku (~$5–8) stays the same-day option when no GPU is available (§12).
- **Knowledge-plane RAM envelope:** 10⁴ chunks ≈ 15–30 MB; 10⁵ ≈ 150–300 MB; 10⁶ ≈ 1.5–2.5 GB. On 15 GB total, the practical ceiling is ~10⁶ chunks with Ollama loaded — well past the realistic personal-corpus range.
- **Disk:** worst case at 10⁵ docs (raw + clean + both stores + models) ≈ 20–30 GB.
- **Driver hygiene (learned the hard way):** prefer Ubuntu's prebuilt signed module packages (`linux-modules-nvidia-*`) over DKMS on stock kernels, keep `linux-headers-$(uname -r)` installed with the HWE meta aligned, and know that the one observed failure mode was a kernel update outrunning the NVIDIA module package. The proprietary stack is pinned away and reinstalled deliberately — never left half-present.
- **Coexistence steady state:** Ollama in VRAM (~3.3 GB for `phi4-mini`) + worker + stores at 10⁵ chunks ⇒ ohara's own footprint ≈ 0.5–1 GB RAM. Extraction batches are GPU-bound — CPU stays free for interactive use; retrieval between batches touches the LLM only for synthesis.

---

## 12. Security and governance

- **Indirect prompt injection:** scraped content is untrusted data. Extraction outputs are schema-validated values, never instructions; synthesis prompts are templated; content found in pages is never executed or followed.
- **SSRF:** the fetcher resolves DNS, blocks private/loopback/link-local targets (config override), allows only `http`/`https`, **re-validates every redirect hop (max 5)**, and connects to the validated resolved IP — pinning DNS for the connection closes the rebinding window.
- **Stored HTML is data:** raw HTML is never re-rendered in a browser context; cleaning strips scripts before Markdown conversion.
- **Data egress:** cloud LLM calls send private scraped content off-machine — **opt-in** (`cloud_llm_enabled = false` by default; local Ollama is the default path and nothing leaves the machine). The `Llm` port's cost/token counters double as the egress audit.
- **At rest:** SQLite + Ladybug + `data/` are plain files; full-disk encryption is the user's OS-level concern (documented, not implemented).
- **Backups — never copy a live store file.** `ohara backup` coordinates all three artifacts: drain workers → `wal_checkpoint(TRUNCATE)` → `VACUUM INTO` → close the Ladybug handle (clean close = consistent file) → copy the knowledge file + `data/` → reopen. SQLite and Ladybug snapshots must be captured in the same quiesced window or cross-store consistency is lost.

---

## 13. Observability

- `tracing` spans per job/stage with `doc_id`; `RUST_LOG` filtered by module.
- `stage_events` is the audit table of record (transitions, retries, errors, panics).
- Counters exported via a metrics handle: docs/sec per stage, tokens in/out, cost estimate per doc, HNSW tombstone ratio, queue depth per stage, `er_review` pending count, documents due for re-crawl, `data/raw` disk usage (against the retention budget, §7.9).

---

## 14. Testing strategy

| Layer | Location | Notes |
| :--- | :--- | :--- |
| Unit | in-module `#[cfg(test)]` | `text.rs` exhaustive; chunker golden-file tests; **URL normalization fixtures** |
| Port contract | `tests/ports/<port>/` | Same suite against every impl + fakes; error-mapping tests; **vector-collection isolation**; `fold_entity` fixture graphs |
| Integration | `tests/integration/` | End-to-end via the public API only — canned fetcher, local embedder, tiny fixture corpus; also validates that module boundaries hold; includes the **FTS trigger-sync** and **deletion-intent** flows |
| Eval | golden set | 50 queries; recall@20/MRR per path; rerank delta; gate for retrieval changes *and* HNSW knobs |

---

## 15. Build order

1. Scaffold crate (module tree, `config.rs`, migrations, worker-loop skeleton)
2. Control store: documents + jobs with lease claiming + stage chaining
3. Fetch ladder leg 1 (HTTP) + `sites` table + robots/politeness + Stages 1–2 (clean, dedup, quality + language gate)
4. Chunker + local embedder + vector collections + `chunks_fts` — **landed; the Ladybug build gates (§2) passed empirically** (MVCC reads with one writer; fold in one transaction; exact KNN via `array_cosine_similarity` until an HNSW index is swapped in)
5. Retrieval baseline: FTS5 + vector + rerank (no graph) — **landed, measured on the golden set** (50 queries, synthetic 48-chunk corpus, relevance true by construction): BM25 recall@20 = 1.000, vector recall@20 = 1.000, fused MRR = 1.000 with the real pinned models (rerank delta +0.000 — fusion order was already perfect on this set; the harness is the deliverable, the numbers the baseline to beat). Not yet built: the graph path and query entities (step 6), symspell correction, `HyDE`, `Llm` synthesis
6. Stage 4: extraction, entity resolution, graph path
7. Obscura leg + full ladder
8. Eval expansion + ops tooling (`ohara backup` / `prune` / `requeue` / `er merge`) + cost dashboards

Step 5 deliberately precedes graph work: the eval baseline quantifies what Stage 4 adds.

---

## Appendix A — Module conventions (Rust Book ch07)

- `lib.rs` declares `pub mod` planes (backyard style); leaves are private (`mod jobs;`) with `pub(crate)`/`pub(super)` internals (Rust Reference visibility) and facade re-exports (`pub use models::{Document, Job};`).
- Absolute `crate::` paths (the book's stated preference); `super::` only for parent-sibling access.
- `src/bin/*` for extra binaries (`ohara backup`, `ohara er merge`, the re-embed tool); `[features]` gate heavy deps (`obscura`, `onnx-embedder`).
- **Workspace graduation (ch14):** each facade file is the future `lib.rs` of `crates/{control,engine,knowledge,pipeline,…}`; the day-one visibility discipline makes the split mechanical.
- Code style, API design, and lint policy live in [CODE_GUIDE.md](CODE_GUIDE.md) — the Rust API Guidelines and Rust Style Guide applied to ohara.

## Appendix B — Change log

### B.1 — v1 → v2

1. **Embedding layer added** (v1's central omission): pinned model, tokenizer-aligned budgets, per-chunk `embedding_model`, re-embed migration.
2. **Cross-store consistency protocol added:** deterministic IDs, intent-before-write, boot reconciliation, re-chunk/re-crawl/delete flows, HNSW tombstone compaction.
3. **Queue semantics added:** per-stage `jobs` with atomic lease claim, expired-lease reclaim, classification-driven retry/backoff, `DEAD` state.
4. **Schema expanded:** `chunks`, `entities`, `entity_aliases`, `triplets`, `stage_events`, `schema_migrations`; `documents` gains conditional-GET, error, and versioning columns.
5. **Ontology corrected:** Date/Time as property (not node), bounded `CONCEPT`, type-compatibility matrix, `Chunk-[:MENTIONS]->Entity` direction fixed.
6. **Entity resolution specified** (v1's lowercase/vector-merge was unsafe): type-consistent, conservative, union-find, alias tables.
7. **Retrieval hardened:** FTS5 BM25 third path, 50→5 rerank pool, RRF, query-entity method defined, golden-set evaluation harness.
8. **Ports formalized (LSP):** postcondition contracts, capability declarations, vendor-language prohibition, contract-test suites with fakes.
9. **Error architecture specified (ch09):** layered taxonomy with retry classes, outcomes-as-values, panic policy + worker `catch_unwind`, newtype validation.
10. **Ops added:** robots.txt/politeness, SSRF guard, cloud-LLM opt-in governance, backups, observability, cost model.

### B.2 — v2 → v2.1 (post-v2 review)

1. **`VectorSpace`-scoped knowledge port** (P0): one collection per model plus an `EntityNames` collection; `read_model`/`write_model` config makes the §4 migration's atomic read switch expressible; `has_vector` replaces `has_chunk`.
2. **FTS5 schema added** (P0): external-content `chunks_fts` keyed on a `chunks.id` surrogate, trigger-synced in the same transaction; BM25 query specified.
3. **Entity identity redesigned** (P0): uuidv7 surrogate + `UNIQUE(canonical_name, entity_type)` lookup; content-hash IDs restricted to immutable rows; `entity_merges` audit + offline merge protocol (§7.8) + `er_review` queue.
4. **Typed aliases** (P0): `PRIMARY KEY (alias, entity_type)` — homographs coexist; type-blind alias hits are misses; `source` provenance column.
5. **Fact-edge identity defined** (P0): `(subject_id, predicate, object_id)` with `support_count`/`evidence`/`occurrences` aggregation; triplets formally the per-chunk evidence layer.
6. **Queue ordering** (P1): `priority` + `created_at` + uuidv7 tiebreak; `next_attempt_at` makes backoff real; `attempts` counts ended executions, not claims; unused `FAILED` job status removed.
7. **Stage chaining + milestone rewind specified** (P1): next stage's job inserted in the milestone transaction; re-crawl rewinds milestones to the last still-true stage.
8. **Terminal mapping** (P1): `DEAD` ⇒ document `FAILED`; `ohara requeue`; `ARCHIVED` defined.
9. **Deletion intent protocol + FK pragmas** (P1): `deletions` table, `PRAGMA foreign_keys = ON`, `ON DELETE CASCADE`.
10. **Language gate** (P1): `target_languages` at the Stage 2 quality gate.
11. **Derived-data principle** (P1, §7.9): SQLite + `data/` are the system of record; knowledge plane + FTS rebuildable; raw retention budget.
12. **Numeric fixes** (P1): `ef_search = max(128, 2·k)`; timestamp format rule (one UTC format, lexicographic = chronological).
13. **Fallible rerank** (P1): `Result` + degradation policy; `Embedder::embed` `spawn_blocking` contract; `StageError::Transient` carries a boxed source + class; retry-class mapping table.
14. **Ops hardening** (P1/P2): `ohara backup` quiesce procedure; SSRF per-redirect re-validation + DNS pinning; `sites` table + adaptive re-crawl; URL normalization spec; `stage_events` retention.
15. **Doc fixes:** port homes in the tree (`Extractor` → `pipeline/clean.rs`, `QueryNormalizer` → `pipeline/retrieve.rs`), `config.rs` naming, chunk writes via `ON CONFLICT DO UPDATE` (never `OR REPLACE`).
16. **Quantization policy (§11.1):** embedder fp32 default (int8 gated by golden-set recall), reranker int8 default (never fp32); quantization variant is part of `model_id` and gets its own collection.
17. **Reference hardware envelope (§11.2):** pinned deployment profile for the dev machine — `phi4-mini` extraction, CPU/int8 embedder + reranker, knowledge-plane RAM envelope (~10⁶-chunk ceiling on 15 GB).
18. **LLM accelerator, settled after a flip-flop:** the profile briefly went CPU-only to avoid driver coupling; reinstated on GPU once the driver was stable. Both configurations are documented in §11.2.
19. **GPU profile (§11.2):** RTX 4050 6 GB via Ubuntu's prebuilt signed module packages (no DKMS); LLM on GPU (`phi4-mini` ≈ 4–7 h per 15k calls), embedder + reranker stay CPU/int8; driver-hygiene notes recorded (headers alignment, deliberate install); all-CPU documented as the fallback profile.

### B.3 — v2.1 implementation amendments (Phase 4 build)

1. **§2 build gates verified empirically against `lbug` 0.20.2** (2026-09-06): MVCC snapshot reads run concurrently with the single write transaction (a second concurrent writer is refused → `KnowledgeError::Unavailable`, Retry); the §7.8 fold (`CREATE` rewire + `DELETE` edges + node delete in one `BEGIN`/`COMMIT`) works. The bundled engine ships **no HNSW** — vector collections are `FLOAT[dim]` node-table properties with exact in-engine KNN (`array_cosine_similarity`); an HNSW index is a drop-in swap behind the port. `lbug` links OpenSSL at link time (`libssl-dev` build prerequisite).
2. **`KnowledgeStore::upsert_vectors` gained a `doc_id` parameter:** the §7.6 delete flow ("every vector collection") requires the store to know vector→document membership; entity-name vectors pass `""`. The §9 sketch above predates this.
3. **`Embedder` port gained `count_tokens`:** §4 requires chunk budgets measured in the embedder's *tokenizer*; the port owns that so stage code never names a vendor tokenizer.
4. **§7.3 realized as idempotent replay, not a sweep:** the Stage 3 body compares the registry's `(seq, content_hash)` signature — identical → repair missing vectors via `has_vector`; drift → §7.4 delete-first. No separate boot pass for knowledge state (deletion intents + audit retention remain in `control::reconcile`).
5. **Feature gates:** `ladybug` and `onnx-embedder` features (both default-on) isolate the heavy native stacks; `--no-default-features` builds for deployments injecting remote providers through `Worker::with_ports`.
