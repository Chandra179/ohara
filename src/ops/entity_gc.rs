//! Entity garbage collection across the control and knowledge planes (§7.9).
//!
//! The control plane records a grace-period candidate; this operator rechecks
//! the knowledge graph before deleting the entity vector/node, then removes the
//! registry row. The runtime lock serializes the sweep with worker mutations.

use crate::config::Config;
use crate::control;
use crate::knowledge::KnowledgeStore;

/// Summary of one entity garbage-collection sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EntityGcReport {
    /// Unmerged entities inspected for mentions.
    pub entities_examined: usize,
    /// New zero-mention candidates recorded.
    pub candidates_recorded: usize,
    /// Candidates removed because a mention reappeared.
    pub candidates_cancelled: usize,
    /// Entities deleted from both stores and the registry.
    pub entities_deleted: usize,
}

/// Runs one idempotent entity garbage-collection sweep.
///
/// A first sweep records a zero-mention observation. A later sweep deletes the
/// entity after `er.entity_gc_grace_days`, with a final graph recheck immediately
/// before the knowledge-plane deletion. Entities referenced by merge audit rows
/// are not returned by the control-plane candidate query.
///
/// # Errors
/// Returns [`super::OpsError::RuntimeBusy`] when another worker/operator holds
/// the lock, or a control/knowledge failure during the sweep.
pub async fn collect_entity_garbage(config: &Config) -> Result<EntityGcReport, super::OpsError> {
    let (_runtime_lock, db) = super::open_control(config)?;
    let store = crate::runtime::writable_knowledge(config)?;
    collect(&db, store.as_ref(), config.er().entity_gc_grace()).await
}

async fn collect(
    db: &control::ControlDb,
    store: &dyn KnowledgeStore,
    grace: std::time::Duration,
) -> Result<EntityGcReport, super::OpsError> {
    let now = control::now();
    let mut report = EntityGcReport::default();
    for entity_id in control::entity_ids_for_gc(db)? {
        report.entities_examined += 1;
        if store
            .chunks_for_entities(&[entity_id.as_str()])
            .await?
            .is_empty()
        {
            if control::record_entity_gc_candidate(db, &entity_id, &now)? {
                report.candidates_recorded += 1;
            }
        } else if control::cancel_entity_gc_candidate(db, &entity_id)? {
            report.candidates_cancelled += 1;
        }
    }

    let cutoff = control::before(&now, grace)?;
    for candidate in control::due_entity_gc_candidates(db, &cutoff)? {
        // A graph write may have happened after candidate discovery in another
        // embedding, so the final check is part of the deletion contract.
        if !store
            .chunks_for_entities(&[candidate.entity_id.as_str()])
            .await?
            .is_empty()
        {
            if control::cancel_entity_gc_candidate(db, &candidate.entity_id)? {
                report.candidates_cancelled += 1;
            }
            continue;
        }
        if store.delete_entity(&candidate.entity_id).await? {
            if control::execute_entity_gc(db, &candidate.entity_id)? {
                report.entities_deleted += 1;
            }
        } else if control::cancel_entity_gc_candidate(db, &candidate.entity_id)? {
            report.candidates_cancelled += 1;
        }
    }
    Ok(report)
}
