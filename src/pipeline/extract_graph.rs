//! Entity resolution and graph application for Stage 4.
//!
//! Extraction stays responsible for the LLM request and checkpoint. This module
//! owns the second half of the stage: resolving typed surfaces, repairing name
//! vectors, linking mentions, and merging fact evidence into the knowledge
//! plane. Both fresh and replayed triplets use the same implementation.

use crate::control::{self, TripletRow};
use crate::knowledge::{EntityType, FactCaps, KnowledgeError, Predicate, VectorSpace};
use crate::text::{name_similarity, normalize_surface_form};

use super::StageError;
use super::execution::ExtractContext;
use super::extraction_contract::ValidTriplet;

/// Per-run resolution state (§8 Stage 4.3): surfaces resolve to exactly one
/// entity id per run, and every registry/vector write is immediate — so later
/// chunks in the same run see earlier resolutions.
#[derive(Default)]
pub(super) struct ResolutionCache {
    /// `(normalized surface, entity_type)` → entity id.
    resolved: std::collections::HashMap<(String, String), String>,
}

fn knowledge_err(err: &KnowledgeError, attempt: u32) -> StageError {
    StageError::transient(std::io::Error::other(err.to_string()), err.class(), attempt)
}

/// Merges one already-staged triplet row into the graph (§7.3 idempotent
/// re-merge: entity resolution, `:MENTIONS` links, and fact-edge aggregation).
pub(super) fn merge_row(
    ctx: &ExtractContext<'_>,
    row: &TripletRow,
    caps: FactCaps,
    attempt: u32,
    cache: &mut ResolutionCache,
) -> Result<(), StageError> {
    let (Ok(subject_type), Ok(predicate), Ok(object_type)) = (
        row.subject_type.parse::<EntityType>(),
        row.predicate.parse::<Predicate>(),
        row.object_type.parse::<EntityType>(),
    ) else {
        return Err(StageError::fatal(std::io::Error::other(format!(
            "staged triplet {} carries an unknown type/predicate (schema CHECK should prevent this)",
            row.triplet_id
        ))));
    };
    let properties = row
        .properties
        .as_deref()
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok());
    merge_triplet_parts(
        ctx,
        &TripletParts {
            subject: &row.subject,
            subject_type,
            predicate,
            object: &row.object,
            object_type,
            chunk_id: &row.chunk_id,
            properties,
        },
        caps,
        attempt,
        cache,
    )
}

/// Merges one fresh triplet: resolve both endpoints, link `:MENTIONS` both ways
/// (one edge per entity), and aggregate the fact edge.
pub(super) fn merge_triplet(
    ctx: &ExtractContext<'_>,
    triplet: &ValidTriplet,
    chunk_id: &str,
    caps: FactCaps,
    attempt: u32,
    cache: &mut ResolutionCache,
) -> Result<(), StageError> {
    let properties = triplet
        .row
        .properties
        .as_deref()
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok());
    merge_triplet_parts(
        ctx,
        &TripletParts {
            subject: &triplet.row.subject,
            subject_type: triplet.subject_type,
            predicate: triplet.predicate,
            object: &triplet.row.object,
            object_type: triplet.object_type,
            chunk_id,
            properties,
        },
        caps,
        attempt,
        cache,
    )
}

/// Resolves a surface form to an entity id (§8 Stage 4): typed alias hit,
/// same-supertype similarity, or a new stable entity.
fn resolve(
    ctx: &ExtractContext<'_>,
    surface: &str,
    entity_type: EntityType,
    attempt: u32,
    cache: &mut ResolutionCache,
) -> Result<String, StageError> {
    let normalized = normalize_surface_form(surface);
    let key = (normalized.clone(), entity_type.as_str().to_string());
    if let Some(id) = cache.resolved.get(&key) {
        return Ok(id.clone());
    }

    if let Some(id) = control::lookup_alias(ctx.conn, &normalized, entity_type.as_str())? {
        ensure_name_vector(ctx, &id, attempt)?;
        ensure_entity_node(ctx, &id, entity_type, attempt)?;
        cache.resolved.insert(key, id.clone());
        return Ok(id);
    }

    let candidates = similarity_candidates(ctx, &normalized, entity_type, attempt)?;
    if let Some(best) = candidates.first() {
        if let Some(second) = candidates.get(1) {
            let _ = control::er_review_candidate(ctx.conn, &best.0, &second.0, second.1);
        }
        let owner =
            match control::upsert_alias(ctx.conn, &normalized, entity_type.as_str(), &best.0)? {
                Some(other) => {
                    let _ = control::er_review_candidate(ctx.conn, &best.0, &other, best.1);
                    other
                }
                None => best.0.clone(),
            };
        ensure_entity_node(ctx, &owner, entity_type, attempt)?;
        cache.resolved.insert(key, owner.clone());
        return Ok(owner);
    }

    let minted = control::new_entity_id();
    let id = control::ensure_entity(ctx.conn, &minted, &normalized, entity_type.as_str(), None)?;
    control::upsert_alias(ctx.conn, &normalized, entity_type.as_str(), &id)?;
    let vector = embed_name(ctx, &normalized, attempt)?;
    ctx.handle
        .block_on(ctx.knowledge.upsert_vectors(
            VectorSpace::EntityNames,
            "",
            &[id.as_str()],
            &[vector],
        ))
        .map_err(|e| knowledge_err(&e, attempt))?;
    ensure_entity_node(ctx, &id, entity_type, attempt)?;
    cache.resolved.insert(key, id.clone());
    Ok(id)
}

fn similarity_candidates(
    ctx: &ExtractContext<'_>,
    normalized: &str,
    entity_type: EntityType,
    attempt: u32,
) -> Result<Vec<(String, f64)>, StageError> {
    let name_threshold = ctx.config.er().name_sim_threshold();
    let embedding_threshold = ctx.config.er().embedding_sim_threshold();

    let mut scores: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for candidate in control::canonical_names(ctx.conn, entity_type.as_str())? {
        let similarity = name_similarity(
            normalized,
            &normalize_surface_form(&candidate.canonical_name),
        );
        if similarity >= name_threshold {
            scores
                .entry(candidate.entity_id)
                .and_modify(|score| *score = score.max(similarity))
                .or_insert(similarity);
        }
    }
    let vector = embed_name(ctx, normalized, attempt)?;
    let hits = ctx
        .handle
        .block_on(ctx.knowledge.knn(
            VectorSpace::EntityNames,
            &vector,
            ctx.config.pipeline().entity_candidate_k(),
            &crate::knowledge::ChunkFilter {},
        ))
        .map_err(|e| knowledge_err(&e, attempt))?;
    for hit in hits {
        let Some(owner_type) = control::entity_type_of(ctx.conn, &hit.id)? else {
            continue;
        };
        if owner_type != entity_type.as_str() {
            continue;
        }
        if f64::from(hit.score) >= embedding_threshold {
            scores
                .entry(hit.id)
                .and_modify(|score| *score = score.max(f64::from(hit.score)))
                .or_insert(f64::from(hit.score));
        }
    }
    let mut ranked: Vec<(String, f64)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(ranked)
}

fn embed_name(
    ctx: &ExtractContext<'_>,
    normalized: &str,
    attempt: u32,
) -> Result<Vec<f32>, StageError> {
    let mut vectors = ctx.embedder.embed(&[normalized]).map_err(|e| {
        StageError::transient(std::io::Error::other(e.to_string()), e.class(), attempt)
    })?;
    vectors.pop().ok_or_else(|| {
        StageError::permanent(format!(
            "embedder returned no vector for {normalized:?} (§9 contract broken)"
        ))
    })
}

fn ensure_name_vector(
    ctx: &ExtractContext<'_>,
    entity_id: &str,
    attempt: u32,
) -> Result<(), StageError> {
    let present = ctx
        .handle
        .block_on(
            ctx.knowledge
                .has_vector(VectorSpace::EntityNames, entity_id),
        )
        .map_err(|e| knowledge_err(&e, attempt))?;
    if present {
        return Ok(());
    }
    let name = control::canonical_name(ctx.conn, entity_id)
        .map_err(StageError::fatal)?
        .ok_or_else(|| {
            StageError::permanent(format!(
                "entity {entity_id:?} has no canonical name in the control registry"
            ))
        })?;
    let vector = embed_name(ctx, &name, attempt)?;
    ctx.handle
        .block_on(ctx.knowledge.upsert_vectors(
            VectorSpace::EntityNames,
            "",
            &[entity_id],
            &[vector],
        ))
        .map_err(|e| knowledge_err(&e, attempt))?;
    Ok(())
}

fn ensure_entity_node(
    ctx: &ExtractContext<'_>,
    entity_id: &str,
    entity_type: EntityType,
    attempt: u32,
) -> Result<(), StageError> {
    let canonical_name = control::canonical_name(ctx.conn, entity_id)
        .map_err(StageError::fatal)?
        .ok_or_else(|| {
            StageError::fatal(format!(
                "entity {entity_id:?} has no canonical name in the control registry"
            ))
        })?;
    ctx.handle
        .block_on(
            ctx.knowledge
                .upsert_entity(&crate::knowledge::EntityRecord {
                    entity_id: entity_id.to_string(),
                    canonical_name,
                    entity_type,
                    subtype: None,
                }),
        )
        .map_err(|e| knowledge_err(&e, attempt))
}

fn merge_triplet_parts(
    ctx: &ExtractContext<'_>,
    parts: &TripletParts<'_>,
    caps: FactCaps,
    attempt: u32,
    cache: &mut ResolutionCache,
) -> Result<(), StageError> {
    let subject_id = resolve(ctx, parts.subject, parts.subject_type, attempt, cache)?;
    let object_id = resolve(ctx, parts.object, parts.object_type, attempt, cache)?;
    for entity_id in [&subject_id, &object_id] {
        ctx.handle
            .block_on(ctx.knowledge.link_mention(parts.chunk_id, entity_id))
            .map_err(|e| knowledge_err(&e, attempt))?;
    }
    ctx.handle
        .block_on(ctx.knowledge.merge_fact(
            &subject_id,
            parts.predicate,
            &object_id,
            parts.chunk_id,
            parts.properties.as_ref(),
            caps,
        ))
        .map_err(|e| knowledge_err(&e, attempt))?;
    Ok(())
}

struct TripletParts<'a> {
    subject: &'a str,
    subject_type: EntityType,
    predicate: Predicate,
    object: &'a str,
    object_type: EntityType,
    chunk_id: &'a str,
    properties: Option<serde_json::Value>,
}
