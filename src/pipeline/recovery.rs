//! Cross-store recovery and boot reconciliation.
//!
//! `SQLite` remains the control-plane owner and Qdrant/FalkorDB remain the
//! knowledge-plane owners. This module owns only the ordering between them for operations
//! that intentionally span both stores: knowledge-first deletion and audit
//! retention.

use std::sync::Mutex;
use std::time::Duration;

use crate::BootError;
use crate::control::{self, ControlDb, ReconcileReport};
use crate::knowledge::KnowledgeStore;

/// Coordinates crash-safe recovery across the control and knowledge planes.
pub(crate) struct Reconciler<'a> {
    conn: &'a Mutex<ControlDb>,
    knowledge: &'a dyn KnowledgeStore,
    retention: Duration,
}

impl<'a> Reconciler<'a> {
    /// Creates a reconciler that locks `SQLite` only for individual operations.
    pub(crate) fn new(
        conn: &'a Mutex<ControlDb>,
        knowledge: &'a dyn KnowledgeStore,
        retention: Duration,
    ) -> Self {
        Self {
            conn,
            knowledge,
            retention,
        }
    }

    /// Replays pending deletion intents, then prunes retained audit rows.
    ///
    /// The knowledge-plane delete always precedes the `SQLite` cascade. If the
    /// process stops between those operations, the intent remains and this same
    /// method safely retries it on the next boot.
    pub(crate) async fn run(&self) -> Result<ReconcileReport, BootError> {
        let now = control::now();
        let deletions_executed = self.replay_deletions().await?;
        let mut report = {
            let conn = self.lock_conn();
            control::reconcile_retention(&conn, &now, self.retention)?
        };
        report.deletions_executed = deletions_executed;
        Ok(report)
    }

    async fn replay_deletions(&self) -> Result<usize, BootError> {
        let intents = {
            let conn = self.lock_conn();
            control::pending_deletions(&conn)?
        };
        let mut executed = 0;
        for intent in intents {
            self.knowledge.delete_doc(&intent.doc_id).await?;
            let deleted = {
                let conn = self.lock_conn();
                control::execute_deletion(&conn, &intent.doc_id)?
            };
            if deleted {
                executed += 1;
            }
        }
        Ok(executed)
    }

    fn lock_conn(&self) -> std::sync::MutexGuard<'_, ControlDb> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
