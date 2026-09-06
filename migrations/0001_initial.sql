-- ohara control-plane schema, v2.1 — ARCHITECTURE.md §5 verbatim.
-- schema_migrations is bootstrapped by control/db.rs (the runner owns its own table);
-- every other table, index, virtual table, and trigger lives here.
--
-- Rules encoded below (§5 notes):
--   * chunk/triplet writes use ON CONFLICT DO UPDATE — never INSERT OR REPLACE
--     (REPLACE churns the chunks.id surrogate and the FTS rowid mapping).
--   * stage_events has no FK — the audit trail outlives deleted rows.
--   * chunks.id is the FTS5 external-content key; never expose it outside SQLite.

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
        ('SCRAPED','CLEANED','VECTORIZED','INDEXED',
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
    updated_at       TIMESTAMP DEFAULT CURRENT_TIMESTAMP
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
