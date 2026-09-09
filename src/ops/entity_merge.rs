//! Offline entity-resolution merge orchestration. The module owns the
//! control-before-knowledge sequencing while the facade owns shared operator
//! locking and error/report types.

use crate::config::Config;
#[cfg(feature = "ladybug")]
use crate::control;

use super::{EntityMergeReport, OpsError, open_control};

/// Executes the offline entity-resolution merge queue.
///
/// The runtime lock makes the operator mutation exclusive with the worker. The
/// control-plane audit is written before each Ladybug fold, and every existing
/// audit row is replayed first so an interrupted cross-store operation heals on
/// the next invocation. Pending candidates choose the entity with the higher
/// `:MENTIONS` degree; ties use the older control-plane row and finally the
/// stable id for deterministic ordering.
///
/// # Errors
/// [`OpsError::RuntimeBusy`] if the worker is active, [`OpsError::Control`] for
/// invalid control-plane state, [`OpsError::Knowledge`] for a fold failure, or
/// [`OpsError::KnowledgeFeatureDisabled`] without the embedded store feature.
pub async fn merge_entities(config: &Config) -> Result<EntityMergeReport, OpsError> {
    let (_runtime_lock, db) = open_control(config)?;
    #[cfg(feature = "ladybug")]
    {
        let store = crate::knowledge::LadybugStore::open(
            &config.data_dir().join("ladybug"),
            config.embedder().dim(),
        )?;
        execute_entity_merges(&db, &store).await
    }
    #[cfg(not(feature = "ladybug"))]
    {
        let _ = db;
        Err(OpsError::KnowledgeFeatureDisabled)
    }
}

#[cfg(feature = "ladybug")]
use crate::knowledge::KnowledgeStore;

#[cfg(feature = "ladybug")]
pub(crate) async fn execute_entity_merges(
    db: &control::ControlDb,
    store: &dyn KnowledgeStore,
) -> Result<EntityMergeReport, OpsError> {
    let mut report = EntityMergeReport::default();

    for merge in control::entity_merges(db)? {
        store.fold_entity(&merge.loser_id, &merge.winner_id).await?;
        report.folds_replayed += 1;
    }

    for review in control::pending_er_reviews(db)? {
        report.reviews_examined += 1;
        let entity_a = control::resolve_entity(db, &review.entity_a)?;
        let entity_b = control::resolve_entity(db, &review.entity_b)?;
        if entity_a == entity_b {
            control::mark_er_review_merged(db, review.id)?;
            continue;
        }

        let details_a = required_entity(db, &entity_a)?;
        let details_b = required_entity(db, &entity_b)?;
        let mentions_a = store.chunks_for_entities(&[entity_a.as_str()]).await?.len();
        let mentions_b = store.chunks_for_entities(&[entity_b.as_str()]).await?.len();
        let (winner, loser) =
            choose_merge_direction(&details_a, mentions_a, &details_b, mentions_b);

        if control::record_entity_merge(db, loser, winner, ER_MERGE_REASON)? {
            report.merges_recorded += 1;
        }
        store.fold_entity(loser, winner).await?;
        report.folds_replayed += 1;
    }

    Ok(report)
}

#[cfg(feature = "ladybug")]
fn required_entity(
    db: &control::ControlDb,
    entity_id: &str,
) -> Result<control::EntityDetails, OpsError> {
    control::entity_details(db, entity_id)?.ok_or_else(|| {
        OpsError::Control(control::DbError::EntityNotFound {
            entity_id: entity_id.to_string(),
        })
    })
}

#[cfg(feature = "ladybug")]
fn choose_merge_direction<'a>(
    a: &'a control::EntityDetails,
    mentions_a: usize,
    b: &'a control::EntityDetails,
    mentions_b: usize,
) -> (&'a str, &'a str) {
    if mentions_a > mentions_b
        || (mentions_a == mentions_b
            && (a.created_at.as_str(), a.entity_id.as_str())
                <= (b.created_at.as_str(), b.entity_id.as_str()))
    {
        (&a.entity_id, &b.entity_id)
    } else {
        (&b.entity_id, &a.entity_id)
    }
}

#[cfg(feature = "ladybug")]
const ER_MERGE_REASON: &str = "offline er merge";
