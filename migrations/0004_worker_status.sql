-- Durable worker lifecycle and heartbeat projection (§13 operations).
-- Worker ids are unique per boot; old rows remain as crash evidence and are
-- classified stale by the runtime readiness check.
CREATE TABLE IF NOT EXISTS worker_status (
    worker_id          TEXT PRIMARY KEY,
    process_id         INTEGER NOT NULL,
    state              TEXT NOT NULL CHECK(state IN
        ('STARTING','READY','RUNNING','STOPPING','STOPPED','FAILED')),
    started_at         TIMESTAMP NOT NULL,
    last_heartbeat_at  TIMESTAMP NOT NULL,
    current_stage      TEXT,
    current_job_id     TEXT,
    last_error         TEXT,
    updated_at         TIMESTAMP NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_worker_status_heartbeat
    ON worker_status(last_heartbeat_at DESC);
