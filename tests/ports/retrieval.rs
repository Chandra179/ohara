use ohara::pipeline::{IdentityReranker, Reranker, ScoredChunk};

#[tokio::test]
async fn identity_reranker_is_deterministic_and_sorted() {
    let reranker = IdentityReranker;
    let port: &dyn Reranker = &reranker;
    let ranked = port
        .rerank(
            "query",
            vec![
                ScoredChunk {
                    chunk_id: "b".to_string(),
                    text: "second".to_string(),
                    score: 0.5,
                },
                ScoredChunk {
                    chunk_id: "a".to_string(),
                    text: "first".to_string(),
                    score: 0.5,
                },
                ScoredChunk {
                    chunk_id: "c".to_string(),
                    text: "best".to_string(),
                    score: 0.9,
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        ranked
            .iter()
            .map(|chunk| chunk.chunk_id.as_str())
            .collect::<Vec<_>>(),
        ["c", "a", "b"]
    );
}
