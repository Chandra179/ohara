//! Stage 4 control-plane facade.
//!
//! The entity registry, triplet checkpoint, review queue, and garbage-collection
//! lifecycle are split into focused child modules. This facade keeps their
//! public control-plane contract flat while `SQLite` remains the sole owner.

mod gc;
mod merge;
mod registry;
mod review;
mod triplets;

pub use gc::{
    EntityGcCandidate, cancel_entity_gc_candidate, due_entity_gc_candidates, entity_ids_for_gc,
    execute_entity_gc, record_entity_gc_candidate,
};
pub use merge::{
    EntityDetails, EntityMerge, entity_details, entity_merges, record_entity_merge, resolve_entity,
};
pub use registry::{
    NameCandidate, canonical_names, ensure_entity, entity_type_of, lookup_alias,
    lookup_alias_all_types, upsert_alias,
};
pub(crate) use registry::{canonical_name, new_entity_id};
pub use review::{ErReview, er_review_candidate, mark_er_review_merged, pending_er_reviews};
pub use triplets::{
    NewTriplet, TripletRow, chunks_without_triplets, stage_triplets, triplets_of_doc,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use rusqlite::Connection;

    use super::super::documents::replace_chunks;
    use super::super::testing::{boot_raw as boot, seed_doc_raw as seed_doc};
    use super::super::{NewChunkRow, Stage};
    use super::*;

    fn triplet(chunk_id: &str, subject: &str, predicate: &str, object: &str) -> NewTriplet {
        NewTriplet {
            triplet_id: format!("id-{chunk_id}-{subject}-{predicate}-{object}"),
            chunk_id: chunk_id.to_string(),
            subject: subject.to_string(),
            subject_type: "PERSON".to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            object_type: "LOCATION".to_string(),
            properties: None,
            model: "gemma3:1b".to_string(),
        }
    }

    fn seed_chunks(conn: &Connection, doc_id: &str, texts: &[&str]) -> Vec<String> {
        let rows: Vec<NewChunkRow> = texts
            .iter()
            .enumerate()
            .map(|(seq, text)| NewChunkRow {
                chunk_id: crate::text::sha256_hex(&format!("{doc_id}:{seq}")),
                seq: i64::try_from(seq).unwrap_or(i64::MAX),
                header_path: "h".to_string(),
                text: text.to_string(),
                embed_text: text.to_string(),
                token_count: 10,
                embedding_model: "bge-small-en-v1.5".to_string(),
                content_hash: crate::text::sha256_hex(text),
            })
            .collect();
        let ids = rows.iter().map(|r| r.chunk_id.clone()).collect();
        replace_chunks(conn, doc_id, &rows).unwrap();
        ids
    }

    #[test]
    fn stage_triplets_returns_only_fresh_rows_and_replays_are_noops() {
        let conn = boot();
        let doc_id = seed_doc(&conn, "d");
        let chunk = seed_chunks(&conn, &doc_id, &["text"])[0].clone();
        let row = triplet(&chunk, "Ada", "LOCATED_IN", "London");

        let fresh = stage_triplets(&conn, vec![row.clone()]).unwrap();
        assert_eq!(fresh.len(), 1);
        // §7.1 replay: the same row is invisible on a second pass.
        let replayed = stage_triplets(&conn, vec![row]).unwrap();
        assert!(replayed.is_empty(), "cost cache is never paid twice (§7.7)");
        // The §6 one-row invariant shape: no duplicate rows exist.
        let count: i64 = conn
            .query_row("SELECT count(*) FROM triplets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn chunks_without_triplets_skips_covered_chunks() {
        let conn = boot();
        let doc_id = seed_doc(&conn, "d");
        let ids = seed_chunks(&conn, &doc_id, &["covered", "pending"]);
        stage_triplets(&conn, vec![triplet(&ids[0], "A", "LOCATED_IN", "B")]).unwrap();

        let pending = chunks_without_triplets(&conn, &doc_id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].chunk_id, ids[1]);
        assert!(
            chunks_without_triplets(&conn, "missing-doc")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn entity_registry_merges_on_the_identity_key() {
        let conn = boot();
        let first = ensure_entity(&conn, "e1", "Ada Lovelace", "PERSON", None).unwrap();
        let again = ensure_entity(&conn, "e2", "Ada Lovelace", "PERSON", Some("math")).unwrap();
        assert_eq!(first, "e1");
        assert_eq!(again, "e1", "the identity key, not the passed id, decides");
        // Same name, different type: a different entity (§8 Jordan rule).
        let other = ensure_entity(&conn, "e3", "Ada Lovelace", "CONCEPT", None).unwrap();
        assert_eq!(other, "e3");
        let (name, subtype): (String, Option<String>) = conn
            .query_row(
                "SELECT canonical_name, subtype FROM entities WHERE entity_id = 'e3'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "Ada Lovelace");
        assert_eq!(subtype, None);
    }

    #[test]
    fn typed_alias_hits_are_exact_and_homographs_coexist() {
        let conn = boot();
        // Aliases carry an FK to their entity — the registry rows come first.
        ensure_entity(&conn, "p1", "Jordan Peele", "PERSON", None).unwrap();
        ensure_entity(&conn, "l1", "Jordan", "LOCATION", None).unwrap();
        ensure_entity(&conn, "p2", "Michael Jordan", "PERSON", None).unwrap();
        assert_eq!(lookup_alias(&conn, "jordan", "PERSON").unwrap(), None);
        assert_eq!(
            upsert_alias(&conn, "jordan", "PERSON", "p1").unwrap(),
            None,
            "fresh alias"
        );
        assert_eq!(
            lookup_alias(&conn, "jordan", "PERSON").unwrap().as_deref(),
            Some("p1")
        );
        // Homograph (§8): same alias, different type — an independent row.
        assert_eq!(
            upsert_alias(&conn, "jordan", "LOCATION", "l1").unwrap(),
            None
        );
        assert_eq!(
            lookup_alias(&conn, "jordan", "LOCATION")
                .unwrap()
                .as_deref(),
            Some("l1")
        );
        // A conflicting same-type mapping reports the existing owner (§7.8).
        assert_eq!(
            upsert_alias(&conn, "jordan", "PERSON", "p2")
                .unwrap()
                .as_deref(),
            Some("p1")
        );
    }

    #[test]
    fn alias_lookup_across_types_returns_every_variant() {
        let conn = boot();
        ensure_entity(&conn, "p1", "Jordan Peele", "PERSON", None).unwrap();
        ensure_entity(&conn, "l1", "Jordan", "LOCATION", None).unwrap();
        ensure_entity(&conn, "r1", "Jordan River", "LOCATION", None).unwrap();
        upsert_alias(&conn, "jordan", "PERSON", "p1").unwrap();
        upsert_alias(&conn, "jordan", "LOCATION", "l1").unwrap();
        upsert_alias(&conn, "jordan river", "LOCATION", "r1").unwrap();

        // §8 Stage 5.2: a homograph yields all its type-variants; ordering is
        // unspecified (the set is what matters), so compare sorted.
        let mut ids = lookup_alias_all_types(&conn, "jordan").unwrap();
        ids.sort();
        assert_eq!(ids, ["l1", "p1"]);
        assert!(lookup_alias_all_types(&conn, "nobody").unwrap().is_empty());
    }

    #[test]
    fn canonical_names_stay_within_the_supertype() {
        let conn = boot();
        ensure_entity(&conn, "p1", "Jordan", "PERSON", None).unwrap();
        ensure_entity(&conn, "l1", "Jordan", "LOCATION", None).unwrap();
        let people = canonical_names(&conn, "PERSON").unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].entity_id, "p1");
    }

    #[test]
    fn er_review_dedups_pending_pairs_in_either_order() {
        let conn = boot();
        assert!(er_review_candidate(&conn, "a", "b", 0.9).unwrap());
        assert!(!er_review_candidate(&conn, "b", "a", 0.9).unwrap(), "dup");
        assert!(!er_review_candidate(&conn, "a", "a", 0.9).unwrap(), "self");
        let pending: i64 = conn
            .query_row(
                "SELECT count(*) FROM er_review WHERE status = 'PENDING'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1);
    }

    #[test]
    fn entity_merge_remaps_aliases_and_is_replay_safe() {
        let conn = boot();
        ensure_entity(&conn, "loser", "Ada", "PERSON", None).unwrap();
        ensure_entity(&conn, "winner", "Ada Lovelace", "PERSON", None).unwrap();
        upsert_alias(&conn, "ada", "PERSON", "loser").unwrap();
        upsert_alias(&conn, "ada lovelace", "PERSON", "winner").unwrap();
        assert!(er_review_candidate(&conn, "loser", "winner", 0.91).unwrap());

        assert!(record_entity_merge(&conn, "loser", "winner", "test").unwrap());
        assert_eq!(resolve_entity(&conn, "loser").unwrap(), "winner");
        assert_eq!(
            lookup_alias(&conn, "ada", "PERSON").unwrap().as_deref(),
            Some("winner")
        );
        assert!(pending_er_reviews(&conn).unwrap().is_empty());
        assert_eq!(entity_merges(&conn).unwrap().len(), 1);
        assert!(!record_entity_merge(&conn, "loser", "winner", "replay").unwrap());
    }

    #[test]
    fn entity_gc_candidates_preserve_first_observation_and_delete_registry_rows() {
        let conn = boot();
        ensure_entity(&conn, "unused", "Unused", "CONCEPT", None).unwrap();
        assert!(record_entity_gc_candidate(&conn, "unused", "2026-09-01 00:00:00").unwrap());
        assert!(!record_entity_gc_candidate(&conn, "unused", "2026-09-02 00:00:00").unwrap());
        assert_eq!(
            due_entity_gc_candidates(&conn, "2026-10-01 00:00:00").unwrap()[0].zero_since,
            "2026-09-01 00:00:00"
        );
        upsert_alias(&conn, "unused", "CONCEPT", "unused").unwrap();
        assert!(execute_entity_gc(&conn, "unused").unwrap());
        assert!(entity_details(&conn, "unused").unwrap().is_none());
        assert!(
            due_entity_gc_candidates(&conn, "9999-12-31 00:00:00")
                .unwrap()
                .is_empty()
        );
        assert!(!execute_entity_gc(&conn, "unused").unwrap());
    }

    #[test]
    fn entity_gc_candidates_are_cancelled_when_mentions_return() {
        let conn = boot();
        ensure_entity(&conn, "used", "Used", "CONCEPT", None).unwrap();
        assert!(record_entity_gc_candidate(&conn, "used", "2026-09-01 00:00:00").unwrap());
        assert!(cancel_entity_gc_candidate(&conn, "used").unwrap());
        assert!(!cancel_entity_gc_candidate(&conn, "used").unwrap());
        assert!(
            due_entity_gc_candidates(&conn, "2026-10-01 00:00:00")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn entity_ids_are_uuidv7_time_ordered() {
        let a = new_entity_id();
        let b = new_entity_id();
        assert_ne!(a, b);
        assert_eq!(uuid::Uuid::parse_str(&a).unwrap().get_version_num(), 7);
        // `Stage::Extract` exists so the stage's job type is real (§6).
        assert_eq!(Stage::Extract.as_str(), "EXTRACT");
    }
}
