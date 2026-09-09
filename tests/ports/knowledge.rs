use ohara::knowledge::{
    ChunkFilter, EntityRecord, EntityType, FactCaps, KnowledgeStore, LadybugStore, ModelId,
    Predicate, VectorSpace,
};

fn entity(id: &str, name: &str, entity_type: EntityType) -> EntityRecord {
    EntityRecord {
        entity_id: id.to_string(),
        canonical_name: name.to_string(),
        entity_type,
        subtype: None,
    }
}

fn fact_caps() -> FactCaps {
    FactCaps {
        max_evidence: 8,
        max_occurrences: 8,
    }
}

#[tokio::test]
async fn vector_space_isolation_replay_and_delete_contract() {
    let store = LadybugStore::in_memory(3).unwrap();
    let port: &dyn KnowledgeStore = &store;
    let model_a = VectorSpace::Chunks {
        model_id: ModelId::new("model-a"),
    };
    let model_b = VectorSpace::Chunks {
        model_id: ModelId::new("model-b"),
    };
    let entity_names = VectorSpace::EntityNames;
    let vector = vec![vec![1.0, 0.0, 0.0]];

    port.upsert_vectors(model_a.clone(), "doc-a", &["chunk-a"], &vector)
        .await
        .unwrap();
    // Replaying the same write must not create a duplicate hit.
    port.upsert_vectors(model_a.clone(), "doc-a", &["chunk-a"], &vector)
        .await
        .unwrap();
    port.upsert_vectors(model_b.clone(), "doc-b", &["chunk-b"], &vector)
        .await
        .unwrap();
    port.upsert_vectors(entity_names.clone(), "", &["entity-a"], &vector)
        .await
        .unwrap();

    let hits = port
        .knn(model_a.clone(), &[1.0, 0.0, 0.0], 10, &ChunkFilter {})
        .await
        .unwrap();
    assert_eq!(hits.iter().filter(|hit| hit.id == "chunk-a").count(), 1);
    assert!(
        port.knn(model_b.clone(), &[1.0, 0.0, 0.0], 10, &ChunkFilter {})
            .await
            .unwrap()
            .iter()
            .any(|hit| hit.id == "chunk-b")
    );
    assert!(
        port.knn(entity_names.clone(), &[1.0, 0.0, 0.0], 10, &ChunkFilter {})
            .await
            .unwrap()
            .iter()
            .any(|hit| hit.id == "entity-a")
    );

    port.delete_doc("doc-a").await.unwrap();
    assert!(!port.has_vector(model_a, "chunk-a").await.unwrap());
    assert!(port.has_vector(model_b, "chunk-b").await.unwrap());
    assert!(port.has_vector(entity_names, "entity-a").await.unwrap());
}

#[tokio::test]
async fn graph_links_and_facts_are_replay_idempotent_and_traversable() {
    let store = LadybugStore::in_memory(3).unwrap();
    let port: &dyn KnowledgeStore = &store;
    let chunks = VectorSpace::Chunks {
        model_id: ModelId::new("graph-model"),
    };

    port.upsert_vectors(
        chunks.clone(),
        "doc-a",
        &["chunk-a"],
        &[vec![1.0, 0.0, 0.0]],
    )
    .await
    .unwrap();
    port.upsert_vectors(chunks, "doc-b", &["chunk-b"], &[vec![0.0, 1.0, 0.0]])
        .await
        .unwrap();
    for record in [
        entity("alice", "Alice", EntityType::Person),
        entity("acme", "Acme", EntityType::Organization),
        entity("paris", "Paris", EntityType::Location),
    ] {
        port.upsert_entity(&record).await.unwrap();
    }

    port.link_mention("chunk-a", "alice").await.unwrap();
    port.link_mention("chunk-a", "alice").await.unwrap();
    port.link_mention("chunk-b", "acme").await.unwrap();
    port.merge_fact(
        "alice",
        Predicate::Founded,
        "acme",
        "chunk-a",
        Some(&serde_json::json!({"occurred_on": "2010-01-01"})),
        fact_caps(),
    )
    .await
    .unwrap();
    // Replaying the same assertion does not create a second edge or support.
    port.merge_fact(
        "alice",
        Predicate::Founded,
        "acme",
        "chunk-a",
        Some(&serde_json::json!({"occurred_on": "2010-01-01"})),
        fact_caps(),
    )
    .await
    .unwrap();
    port.merge_fact(
        "acme",
        Predicate::LocatedIn,
        "paris",
        "chunk-b",
        None,
        fact_caps(),
    )
    .await
    .unwrap();

    assert_eq!(
        port.chunks_for_entities(&["alice"]).await.unwrap(),
        ["chunk-a"]
    );
    let one_hop = port.facts_within_hops(&["alice"], 1).await.unwrap();
    assert_eq!(one_hop.len(), 1, "one hop returns the direct fact");
    assert_eq!(one_hop[0].support_count, 1, "replay must not double-count");
    let two_hops = port.facts_within_hops(&["alice"], 2).await.unwrap();
    assert_eq!(two_hops.len(), 2, "two hops reaches Acme's location fact");
}

#[tokio::test]
async fn fold_rewires_mentions_and_facts_and_is_safe_to_replay() {
    let store = LadybugStore::in_memory(3).unwrap();
    let port: &dyn KnowledgeStore = &store;
    let chunks = VectorSpace::Chunks {
        model_id: ModelId::new("fold-model"),
    };
    port.upsert_vectors(chunks, "doc-a", &["chunk-a"], &[vec![1.0, 0.0, 0.0]])
        .await
        .unwrap();
    for record in [
        entity("loser", "Old Name", EntityType::Person),
        entity("winner", "Canonical Name", EntityType::Person),
        entity("place", "Jakarta", EntityType::Location),
    ] {
        port.upsert_entity(&record).await.unwrap();
    }
    port.link_mention("chunk-a", "loser").await.unwrap();
    // Seed the winner with the same identities to exercise collision-aware
    // rewiring rather than only the simple loser-to-winner path.
    port.link_mention("chunk-a", "winner").await.unwrap();
    port.merge_fact(
        "loser",
        Predicate::LocatedIn,
        "place",
        "chunk-a",
        None,
        fact_caps(),
    )
    .await
    .unwrap();
    port.merge_fact(
        "winner",
        Predicate::LocatedIn,
        "place",
        "chunk-b",
        None,
        fact_caps(),
    )
    .await
    .unwrap();

    port.fold_entity("loser", "winner").await.unwrap();
    port.fold_entity("loser", "winner").await.unwrap();

    assert!(
        port.chunks_for_entities(&["loser"])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        port.chunks_for_entities(&["winner"]).await.unwrap(),
        ["chunk-a"]
    );
    let facts = port.facts_within_hops(&["winner"], 1).await.unwrap();
    assert_eq!(facts.len(), 1, "fold replay must not duplicate the fact");
    assert_eq!(facts[0].subject_id, "winner");
    assert_eq!(facts[0].object_id, "place");
    assert_eq!(facts[0].support_count, 2, "fold preserves both supports");
    assert_eq!(
        facts[0]
            .properties
            .as_ref()
            .and_then(|properties| properties.get("evidence"))
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len),
        2,
        "fold unions evidence from colliding edges"
    );
}

#[tokio::test]
async fn delete_doc_removes_document_chunks_and_mentions_but_keeps_other_docs() {
    let store = LadybugStore::in_memory(3).unwrap();
    let port: &dyn KnowledgeStore = &store;
    let chunks = VectorSpace::Chunks {
        model_id: ModelId::new("delete-model"),
    };
    port.upsert_vectors(
        chunks.clone(),
        "doc-a",
        &["chunk-a"],
        &[vec![1.0, 0.0, 0.0]],
    )
    .await
    .unwrap();
    port.upsert_vectors(
        chunks.clone(),
        "doc-b",
        &["chunk-b"],
        &[vec![0.0, 1.0, 0.0]],
    )
    .await
    .unwrap();
    port.upsert_entity(&entity("alice", "Alice", EntityType::Person))
        .await
        .unwrap();
    port.link_mention("chunk-a", "alice").await.unwrap();
    port.link_mention("chunk-b", "alice").await.unwrap();

    port.delete_doc("doc-a").await.unwrap();

    assert!(!port.has_vector(chunks.clone(), "chunk-a").await.unwrap());
    assert!(port.has_vector(chunks, "chunk-b").await.unwrap());
    assert_eq!(
        port.chunks_for_entities(&["alice"]).await.unwrap(),
        ["chunk-b"]
    );
}
