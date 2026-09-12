//! Stage 4 — Extract graph (§8, §15 step 6): LLM triplet extraction → matrix
//! validation → `triplets` staging (the §7.7 cost cache) → conservative
//! type-consistent entity resolution → graph writes (`(:Chunk)-[:MENTIONS]->(:Entity)`
//! links + fact-edge aggregation). The cross-store order follows §7: triplets
//! commit in `SQLite` first (the intent), entity registry rows and the knowledge
//! plane after, the milestone only on `Ok`.
//!
//! Two properties make the stage replay-safe without re-paying the LLM:
//! - extraction runs only for chunks without triplets (§7.7 coverage);
//! - staged triplets are re-merged into the graph on every run (§7.3: merges are
//!   idempotent — a crash between staging and the graph write heals on resume).

use crate::control::{self, ChunkText, ClaimedJob, NewTriplet};
use crate::knowledge::FactCaps;
use crate::llm::{CompletionRequest, LlmError};

use super::StageError;
use super::StageOutcome;
use super::execution::ExtractContext;
use super::extract_graph::{self, ResolutionCache};
use super::extraction_contract::{self, extraction_prompt, parse_triplets, triplets_schema};
use super::usage::{CompletionContext, UsageError};

fn attempt_of(job: &ClaimedJob) -> u32 {
    u32::try_from(job.attempts().saturating_add(1)).unwrap_or(u32::MAX)
}

/// Records an intra-stage drop (§8: matrix violations and unparsable responses
/// are logged, never silently discarded). `SKIP` is a stage-internal outcome —
/// the §5 vocabulary lists the *worker's* outcomes; §8 requires these drops on
/// the audit trail.
fn record_skip(ctx: &ExtractContext<'_>, job: &ClaimedJob, detail: &str) {
    let _ = control::record_event(
        ctx.conn,
        Some(job.doc_id()),
        Some(job.job_id()),
        Some("EXTRACT"),
        "SKIP",
        Some(detail),
    );
}

/// Runs Stage 4 for one claimed job (§8). The §7.7 coverage checkpoint makes
/// resumption free: extraction runs only for uncovered chunks, and staged
/// triplets are re-merged into the graph idempotently (§7.3) so an interrupted
/// run heals without re-paying the LLM.
///
/// # Errors
/// [`StageError`] classified per §10.
pub(super) fn run(ctx: &ExtractContext<'_>, job: &ClaimedJob) -> Result<StageOutcome, StageError> {
    let attempt = attempt_of(job);
    if control::get(ctx.conn, job.doc_id())?.is_none() {
        return Err(StageError::permanent(format!(
            "document {} vanished mid-run (§10 invariant)",
            job.doc_id()
        )));
    }

    let model = ctx.config.llm().extraction_model().to_string();
    let caps = FactCaps {
        max_evidence: ctx.config.er().max_evidence(),
        max_occurrences: ctx.config.er().max_occurrences(),
    };
    let mut cache = ResolutionCache::default();

    // §7.3 resume heal: re-merge everything already staged (idempotent no-op on
    // a clean run — edges dedup by identity). A crash between the triplets
    // transaction and the graph writes therefore always repairs.
    for row in control::triplets_of_doc(ctx.conn, job.doc_id())? {
        extract_graph::merge_row(ctx, &row, caps, attempt, &mut cache)?;
    }

    // §7.7 coverage: pay the LLM only for uncovered chunks.
    let pending = control::chunks_without_triplets(ctx.conn, job.doc_id())?;
    for chunk in &pending {
        extract_chunk(ctx, job, chunk, &model, caps, attempt, &mut cache)?;
    }
    Ok(StageOutcome::Advance)
}

/// One chunk's extraction pass: LLM → parse → matrix validation → staging →
/// entity resolution → graph writes for the freshly staged rows only.
fn extract_chunk(
    ctx: &ExtractContext<'_>,
    job: &ClaimedJob,
    chunk: &ChunkText,
    model: &str,
    caps: FactCaps,
    attempt: u32,
    cache: &mut ResolutionCache,
) -> Result<(), StageError> {
    let request = CompletionRequest {
        model: model.to_string(),
        prompt: extraction_prompt(&chunk.text, ctx.config.pipeline().max_triplets_per_chunk()),
        max_tokens: None,
        temperature: 0.0,
        json_schema: Some(triplets_schema()),
    };
    let response = match ctx.handle.block_on(super::usage::complete(
        ctx.config,
        ctx.conn,
        ctx.llm,
        request,
        CompletionContext {
            operation: "extract",
            doc_id: Some(job.doc_id()),
            job_id: Some(job.job_id()),
        },
    )) {
        Ok(response) => response,
        Err(UsageError::Provider(LlmError::InvalidResponse(detail))) => {
            // §8: a malformed response skips the chunk (temperature-0
            // regeneration would reproduce it); the run continues — the chunk
            // stays uncovered and is retried on the next execution.
            record_skip(ctx, job, &format!("chunk {}: {detail}", chunk.chunk_id));
            return Ok(());
        }
        Err(UsageError::Provider(other)) => {
            return Err(StageError::transient(
                std::io::Error::other(other.to_string()),
                other.class(),
                attempt,
            ));
        }
        Err(UsageError::Ledger(error)) => return Err(StageError::fatal(error)),
    };
    let parsed = match parse_triplets(
        &response.text,
        ctx.config.pipeline().max_triplets_per_chunk(),
    ) {
        Ok(parsed) => parsed,
        Err(e) => {
            record_skip(ctx, job, &format!("chunk {}: {e}", chunk.chunk_id));
            return Ok(());
        }
    };

    let validation = extraction_contract::validate_triplets(parsed, &chunk.chunk_id, model);
    for rejected in &validation.rejected {
        record_skip(ctx, job, &rejected.detail);
    }
    if validation.valid.is_empty() {
        return Ok(());
    }
    let rows: Vec<NewTriplet> = validation.valid.iter().map(|v| v.row.clone()).collect();
    // §7.2 intent-before-write: the cost cache commits before the knowledge
    // plane is touched; only freshly staged rows merge below (replays are no-ops).
    let fresh = control::stage_triplets(ctx.conn, rows)?;
    let fresh_ids: std::collections::HashSet<&str> =
        fresh.iter().map(|t| t.triplet_id.as_str()).collect();
    for triplet in validation
        .valid
        .into_iter()
        .filter(|v| fresh_ids.contains(v.row.triplet_id.as_str()))
    {
        extract_graph::merge_triplet(ctx, &triplet, &chunk.chunk_id, caps, attempt, cache)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::runtime::Handle;

    use super::*;
    use crate::config::Config;
    use crate::control::testing::seed_doc;
    use crate::control::{self, ClaimedJob, ControlDb, NewChunkRow, Stage};
    use crate::knowledge::{InMemoryKnowledge, KnowledgeStore, VectorSpace};
    use crate::llm::CompletionResponse;
    use crate::pipeline::test_support::FakeEmbedder;
    use crate::text::sha256_hex;

    const NOW: &str = "2026-09-06 12:00:00";

    /// An LLM fake whose queue returns one scripted response per call (in chunk
    /// order); every response also feeds the usage counters like a real client.
    struct FakeLlm {
        responses: std::sync::Mutex<Vec<Result<String, crate::llm::LlmError>>>,
        calls: AtomicUsize,
    }

    impl FakeLlm {
        fn new(responses: Vec<Result<String, crate::llm::LlmError>>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl crate::llm::Llm for FakeLlm {
        fn provider_name(&self) -> &'static str {
            "fake"
        }

        async fn complete(
            &self,
            _req: crate::llm::CompletionRequest,
        ) -> Result<CompletionResponse, crate::llm::LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let next = {
                let mut queue = self.responses.lock().unwrap();
                if queue.is_empty() {
                    None
                } else {
                    Some(queue.remove(0))
                }
            };
            let text = match next {
                None => "{\"triplets\":[]}".to_string(),
                Some(Ok(text)) => text,
                Some(Err(e)) => return Err(e),
            };
            Ok(CompletionResponse {
                text,
                prompt_tokens: 10,
                completion_tokens: 20,
            })
        }
    }

    struct Fixture {
        #[allow(dead_code)] // keeps the tempdir alive for the test's duration
        dir: tempfile::TempDir,
        config: Arc<Config>,
        store: std::path::PathBuf,
        doc_id: String,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        let toml_path = dir.path().join("ohara.toml");
        std::fs::write(
            &toml_path,
            format!("data_dir = {:?}\n", dir.path().join("data").display()),
        )
        .unwrap();
        let config = Arc::new(Config::load(Some(&toml_path)).unwrap());
        let store = dir.path().join("store.db");
        let conn = control::connect(&store).unwrap();
        let doc_id = seed_doc(&conn, "a");
        drop(conn);
        Fixture {
            dir,
            config,
            store,
            doc_id,
        }
    }

    fn seed_chunks(conn: &ControlDb, doc_id: &str, texts: &[&str]) {
        let rows: Vec<NewChunkRow> = texts
            .iter()
            .enumerate()
            .map(|(seq, text)| NewChunkRow {
                chunk_id: sha256_hex(&format!("{doc_id}:{seq}")),
                seq: i64::try_from(seq).unwrap_or(i64::MAX),
                header_path: "h".to_string(),
                text: (*text).to_string(),
                embed_text: (*text).to_string(),
                token_count: 20,
                embedding_model: "fake-embedder".to_string(),
                content_hash: sha256_hex(text),
            })
            .collect();
        control::replace_chunks(conn, doc_id, &rows).unwrap();
    }

    fn chunk_ids(conn: &ControlDb, doc_id: &str) -> Vec<String> {
        let mut stmt = conn
            .raw()
            .prepare("SELECT chunk_id FROM chunks WHERE doc_id = ?1 ORDER BY seq")
            .unwrap();
        let rows = stmt.query_map([doc_id], |r| r.get::<_, String>(0)).unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    }

    fn extract_job(conn: &ControlDb, doc_id: &str) -> ClaimedJob {
        control::enqueue(conn, "e-job", doc_id, Stage::Extract, 5, None, NOW).unwrap();
        control::claim_next(conn, Stage::Extract, "w1", NOW, 60)
            .unwrap()
            .unwrap()
    }

    async fn run_stage(
        config: &Arc<Config>,
        store: &std::path::Path,
        job: &ClaimedJob,
        llm: &Arc<FakeLlm>,
        embedder: &Arc<FakeEmbedder>,
        knowledge: &Arc<InMemoryKnowledge>,
    ) -> Result<StageOutcome, StageError> {
        let config = Arc::clone(config);
        let store = store.to_path_buf();
        let job = job.clone();
        let llm = Arc::clone(llm);
        let embedder = Arc::clone(embedder);
        let knowledge = Arc::clone(knowledge);
        tokio::task::spawn_blocking(move || {
            let conn = control::connect(&store).unwrap();
            let handle = Handle::current();
            let ctx = super::super::execution::ExtractContext {
                config: config.as_ref(),
                conn: &conn,
                handle: &handle,
                embedder: embedder.as_ref(),
                knowledge: knowledge.as_ref(),
                llm: llm.as_ref(),
            };
            run(&ctx, &job)
        })
        .await
        .unwrap()
    }

    fn triplet_json(entries: &str) -> String {
        format!("{{\"triplets\":[{entries}]}}")
    }

    fn entities_of(conn: &ControlDb, entity_type: &str) -> Vec<(String, String)> {
        let mut stmt = conn
            .raw()
            .prepare(
                "SELECT entity_id, canonical_name FROM entities WHERE entity_type = ?1 ORDER BY canonical_name",
            )
            .unwrap();
        let rows = stmt
            .query_map([entity_type], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    }

    #[tokio::test]
    async fn full_flow_stages_triplets_and_merges_the_graph() {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        seed_chunks(
            &conn,
            &fx.doc_id,
            &["SQLite is a C library created by D. Richard Hipp."],
        );
        let chunk_id = chunk_ids(&conn, &fx.doc_id)[0].clone();
        let job = extract_job(&conn, &fx.doc_id);
        drop(conn);

        let llm = Arc::new(FakeLlm::new(vec![Ok(triplet_json(
            r#"{"subject":"SQLite","subject_type":"PRODUCT","predicate":"CREATED_BY","object":"D. Richard Hipp","object_type":"PERSON"},
               {"subject":"SQLite","subject_type":"PRODUCT","predicate":"DEPENDS_ON","object":"C","object_type":"CONCEPT"}"#,
        ))]));
        let embedder = Arc::new(FakeEmbedder::new());
        let knowledge = Arc::new(InMemoryKnowledge::default());
        let outcome = run_stage(&fx.config, &fx.store, &job, &llm, &embedder, &knowledge)
            .await
            .unwrap();
        assert_eq!(outcome, StageOutcome::Advance);

        // The cost cache (§7.7): both triplets staged with the extractor model.
        let conn = control::connect(&fx.store).unwrap();
        let (count, model): (i64, String) = conn
            .raw()
            .query_row(
                "SELECT count(*), min(model) FROM triplets WHERE chunk_id = ?1",
                [&chunk_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(model, "gemma3:1b", "§11 prompt-version guard");

        // The registry: three entities with typed aliases.
        let people = entities_of(&conn, "PERSON");
        let products = entities_of(&conn, "PRODUCT");
        let concepts = entities_of(&conn, "CONCEPT");
        assert_eq!(products.len(), 1);
        assert_eq!(products[0].1, "sqlite");
        assert_eq!(people[0].1, "d. richard hipp");
        assert_eq!(concepts[0].1, "c");
        let alias_owner: String = conn
            .raw()
            .query_row(
                "SELECT entity_id FROM entity_aliases WHERE alias = 'sqlite' AND entity_type = 'PRODUCT'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(alias_owner, products[0].0);

        // Entity-name vectors exist (§8 Stage 4.2's embedding path).
        for id in [&products[0].0, &people[0].0, &concepts[0].0] {
            assert!(
                knowledge
                    .has_vector(VectorSpace::EntityNames, id)
                    .await
                    .unwrap(),
                "entity {id} needs a name vector"
            );
        }

        // The graph: two FACT edges, MENTIONS from the chunk to every entity.
        let facts = knowledge.facts();
        assert_eq!(facts.len(), 2, "{facts:?}");
        let mentions = knowledge
            .chunks_for_entities(&[&products[0].0, &people[0].0, &concepts[0].0])
            .await
            .unwrap();
        assert_eq!(mentions, [chunk_id.as_str()]);
    }

    #[tokio::test]
    async fn matrix_violations_are_dropped_and_audited() {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        seed_chunks(
            &conn,
            &fx.doc_id,
            &["Alice lives in Paris and is a building."],
        );
        let job = extract_job(&conn, &fx.doc_id);
        drop(conn);

        // PERSON --LOCATED_IN--> PERSON violates the matrix (§8); the valid
        // PERSON --LOCATED_IN--> LOCATION survives.
        let llm = Arc::new(FakeLlm::new(vec![Ok(triplet_json(
            r#"{"subject":"Alice","subject_type":"PERSON","predicate":"LOCATED_IN","object":"Paris","object_type":"LOCATION"},
               {"subject":"Alice","subject_type":"PERSON","predicate":"LOCATED_IN","object":"Bob","object_type":"PERSON"}"#,
        ))]));
        let embedder = Arc::new(FakeEmbedder::new());
        let knowledge = Arc::new(InMemoryKnowledge::default());
        run_stage(&fx.config, &fx.store, &job, &llm, &embedder, &knowledge)
            .await
            .unwrap();

        let conn = control::connect(&fx.store).unwrap();
        let count: i64 = conn
            .raw()
            .query_row("SELECT count(*) FROM triplets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "the violation never reaches the cache");
        let skips: i64 = conn
            .raw()
            .query_row(
                "SELECT count(*) FROM stage_events WHERE outcome = 'SKIP' AND detail LIKE '%matrix violation%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(skips, 1, "§8: violations are logged, not silent");
        assert_eq!(knowledge.facts().len(), 1);
    }

    #[tokio::test]
    async fn replay_never_repays_the_llm_and_edges_do_not_double_count() {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        seed_chunks(&conn, &fx.doc_id, &["SQLite is written in C."]);
        let job = extract_job(&conn, &fx.doc_id);
        drop(conn);

        let llm = Arc::new(FakeLlm::new(vec![Ok(triplet_json(
            r#"{"subject":"SQLite","subject_type":"PRODUCT","predicate":"DEPENDS_ON","object":"C","object_type":"CONCEPT"}"#,
        ))]));
        let embedder = Arc::new(FakeEmbedder::new());
        let knowledge = Arc::new(InMemoryKnowledge::default());
        run_stage(&fx.config, &fx.store, &job, &llm, &embedder, &knowledge)
            .await
            .unwrap();
        assert_eq!(llm.calls(), 1);

        // §7.3 replay: everything covered → no LLM call; the §7.3 re-merge is a
        // no-op on the already-written graph (§7.1 idempotency).
        run_stage(&fx.config, &fx.store, &job, &llm, &embedder, &knowledge)
            .await
            .unwrap();
        assert_eq!(llm.calls(), 1, "§7.7: extraction is never paid twice");
        let facts = knowledge.facts();
        assert_eq!(facts.len(), 1);
        assert_eq!(
            facts[0].support_count, 1,
            "evidence dedup keeps support at 1"
        );
        let conn = control::connect(&fx.store).unwrap();
        let count: i64 = conn
            .raw()
            .query_row("SELECT count(*) FROM triplets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn surface_forms_resolve_by_name_similarity_within_the_supertype() {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        seed_chunks(
            &conn,
            &fx.doc_id,
            &["PostgreSQL is a database.", "Postgres runs everywhere."],
        );
        let job = extract_job(&conn, &fx.doc_id);
        drop(conn);

        // Both chunks mention the same product under slightly different surface
        // forms; the second resolves to the first via name similarity (§8).
        let llm = Arc::new(FakeLlm::new(vec![
            Ok(triplet_json(
                r#"{"subject":"PostgreSQL","subject_type":"PRODUCT","predicate":"PART_OF","object":"stack","object_type":"CONCEPT"}"#,
            )),
            Ok(triplet_json(
                r#"{"subject":"Postgres","subject_type":"PRODUCT","predicate":"PART_OF","object":"stack","object_type":"CONCEPT"}"#,
            )),
        ]));
        let embedder = Arc::new(FakeEmbedder::new());
        let knowledge = Arc::new(InMemoryKnowledge::default());
        run_stage(&fx.config, &fx.store, &job, &llm, &embedder, &knowledge)
            .await
            .unwrap();

        let conn = control::connect(&fx.store).unwrap();
        let products = entities_of(&conn, "PRODUCT");
        assert_eq!(
            products.len(),
            1,
            "similar surfaces must fold into one entity, got {products:?}"
        );
        assert_eq!(products[0].1, "postgresql");
        // Both surface forms are aliases of the same entity.
        let aliases: i64 = conn
            .raw()
            .query_row(
                "SELECT count(*) FROM entity_aliases WHERE entity_id = ?1 AND entity_type = 'PRODUCT'",
                [&products[0].0],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(aliases, 2);
        // No cross-document review needed: one unambiguous candidate.
        let review: i64 = conn
            .raw()
            .query_row("SELECT count(*) FROM er_review", [], |r| r.get(0))
            .unwrap();
        assert_eq!(review, 0);
        // Both chunks' facts share the same subject id.
        let facts = knowledge.facts();
        assert_eq!(facts.len(), 1, "one fact edge after resolution: {facts:?}");
    }

    #[tokio::test]
    async fn near_tie_candidates_file_an_er_review_row() {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        // Two product surfaces that neither name- nor embedding-match each
        // other (different initials → orthogonal fake vectors, name distance
        // 0.75 < 0.85), but that both name-match a third surface ("postgres",
        // 0.875 ≥ 0.85 twice): an unresolvable near-tie (§8 Stage 4.3).
        seed_chunks(
            &conn,
            &fx.doc_id,
            &[
                "Postgre handles data. Ostgres handles data too.",
                "Postgres is the shorthand.",
            ],
        );
        let job = extract_job(&conn, &fx.doc_id);
        drop(conn);

        let llm = Arc::new(FakeLlm::new(vec![
            Ok(triplet_json(
                r#"{"subject":"Postgre","subject_type":"PRODUCT","predicate":"PART_OF","object":"db","object_type":"CONCEPT"},
                   {"subject":"Ostgres","subject_type":"PRODUCT","predicate":"PART_OF","object":"db","object_type":"CONCEPT"}"#,
            )),
            Ok(triplet_json(
                r#"{"subject":"Postgres","subject_type":"PRODUCT","predicate":"PART_OF","object":"db","object_type":"CONCEPT"}"#,
            )),
        ]));
        let embedder = Arc::new(FakeEmbedder::new());
        let knowledge = Arc::new(InMemoryKnowledge::default());
        run_stage(&fx.config, &fx.store, &job, &llm, &embedder, &knowledge)
            .await
            .unwrap();

        let conn = control::connect(&fx.store).unwrap();
        let products = entities_of(&conn, "PRODUCT");
        assert_eq!(products.len(), 2, "created entities: {products:?}");
        // "postgres" cannot separate the two → a PENDING review row (§8.4.3).
        let pending: i64 = conn
            .raw()
            .query_row(
                "SELECT count(*) FROM er_review WHERE status = 'PENDING'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1);
        // The best candidate still won the alias (deterministic resolution).
        let owner: String = conn
            .raw()
            .query_row(
                "SELECT entity_id FROM entity_aliases WHERE alias = 'postgres'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(products.iter().any(|(id, _)| id == &owner));
    }

    #[tokio::test]
    async fn invalid_llm_responses_skip_the_chunk_and_the_run_advances() {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        seed_chunks(&conn, &fx.doc_id, &["Some text the model fails on."]);
        let job = extract_job(&conn, &fx.doc_id);
        drop(conn);

        let llm = Arc::new(FakeLlm::new(vec![Err(
            crate::llm::LlmError::InvalidResponse("garbage".to_string()),
        )]));
        let embedder = Arc::new(FakeEmbedder::new());
        let knowledge = Arc::new(InMemoryKnowledge::default());
        let outcome = run_stage(&fx.config, &fx.store, &job, &llm, &embedder, &knowledge)
            .await
            .unwrap();
        assert_eq!(outcome, StageOutcome::Advance, "§8: bounded, logged loss");

        let conn = control::connect(&fx.store).unwrap();
        let skips: i64 = conn
            .raw()
            .query_row(
                "SELECT count(*) FROM stage_events WHERE outcome = 'SKIP'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(skips, 1);
        let count: i64 = conn
            .raw()
            .query_row("SELECT count(*) FROM triplets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn llm_unavailability_is_a_transient_failure() {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        seed_chunks(&conn, &fx.doc_id, &["Text."]);
        let job = extract_job(&conn, &fx.doc_id);
        drop(conn);

        let llm = Arc::new(FakeLlm::new(vec![Err(crate::llm::LlmError::Unavailable(
            "down".to_string(),
        ))]));
        let embedder = Arc::new(FakeEmbedder::new());
        let knowledge = Arc::new(InMemoryKnowledge::default());
        let err = run_stage(&fx.config, &fx.store, &job, &llm, &embedder, &knowledge)
            .await
            .unwrap_err();
        assert_eq!(err.class(), crate::Class::Retry);
        assert!(matches!(err, StageError::Transient { .. }), "got {err:?}");
    }

    #[test]
    fn the_matrix_admits_exactly_the_expected_cells() {
        use crate::knowledge::{EntityType as T, Predicate as P};
        let cells = [
            (T::Event, P::LocatedIn, T::Location, true),
            (T::Person, P::LocatedIn, T::Location, true),
            (T::Person, P::LocatedIn, T::Person, false),
            (T::Concept, P::PartOf, T::Concept, true),
            (T::Product, P::CreatedBy, T::Organization, true),
            (T::Location, P::CreatedBy, T::Person, false),
            (T::Organization, P::Caused, T::Event, true),
            (T::Event, P::Affected, T::Product, false),
            (T::Organization, P::ParticipatedIn, T::Event, true),
            (T::Location, P::Produces, T::Product, true),
            (T::Person, P::Founded, T::Organization, true),
            (T::Organization, P::DependsOn, T::Product, true),
            (T::Person, P::DependsOn, T::Person, false),
            (T::Event, P::AssociatedWith, T::Location, true),
        ];
        for (subject, predicate, object, expected) in cells {
            assert_eq!(
                super::super::extraction_contract::compatible(subject, predicate, object),
                expected,
                "{subject:?} --{predicate:?}--> {object:?}"
            );
        }
    }
}
