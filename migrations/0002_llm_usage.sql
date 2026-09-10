-- Durable LLM completion-attempt ledger (§13 observability).
--
-- Token counts can be zero when a provider fails before returning usage
-- metadata. Every row is append-only: retries are separate attempts.
CREATE TABLE IF NOT EXISTS llm_usage (
    attempt_id             INTEGER PRIMARY KEY,
    operation              TEXT NOT NULL, -- extract | synthesis
    doc_id                 TEXT,
    job_id                 TEXT,
    provider               TEXT NOT NULL,
    model                  TEXT NOT NULL,
    outcome                TEXT NOT NULL CHECK(outcome IN ('SUCCEEDED', 'FAILED')),
    prompt_tokens          INTEGER NOT NULL DEFAULT 0,
    completion_tokens      INTEGER NOT NULL DEFAULT 0,
    estimated_cost_micros  INTEGER NOT NULL DEFAULT 0,
    error                  TEXT,
    recorded_at            TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_llm_usage_doc ON llm_usage(doc_id, recorded_at);
CREATE INDEX IF NOT EXISTS idx_llm_usage_job ON llm_usage(job_id, recorded_at);
CREATE INDEX IF NOT EXISTS idx_llm_usage_time ON llm_usage(recorded_at);
