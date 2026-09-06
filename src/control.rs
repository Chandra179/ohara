//! CONTROL PLANE facade (§1.3) — owns all `SQLite` access: documents, the job queue,
//! the audit trail, and the schema. One directory owns the store; schema changes
//! touch one place. Pipeline code sees only this facade.

mod db;
mod documents;
mod jobs;
mod models;

pub use db::{DbError, connect};
pub use jobs::{claim_next, complete, dead, enqueue, record_event, retry};
pub use models::{ClaimedJob, Stage};

pub(crate) use db::{now, now_plus};
