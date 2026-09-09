use ohara::knowledge::{ChunkFilter, KnowledgeStore, LadybugStore, ModelId, VectorSpace};

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
    assert!(!port
        .has_vector(model_a, "chunk-a")
        .await
        .unwrap());
    assert!(port
        .has_vector(model_b, "chunk-b")
        .await
        .unwrap());
    assert!(port
        .has_vector(entity_names, "entity-a")
        .await
        .unwrap());
}
