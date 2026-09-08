//! Stage 3b — Embed, and the [`Embedder`] port (§1.3, §9). The stage body drives
//! §8 Stage 3: chunk (via [`super::chunk`]) → embed (via [`Embedder`]) → registry
//! rows + vector upsert, honoring the §7 cross-store protocol (registry first,
//! knowledge write after, milestone only on `Ok`). The local ONNX implementation
//! ([`LocalEmbedder`], behind the `onnx-embedder` feature) embeds with the pinned
//! `BAAI/bge-small-en-v1.5` (§4); provider swaps (API embedders) implement the
//! same port at equal model.

use crate::Class;
use crate::control::NewChunkRow;
use crate::control::{self, ClaimedJob};
use crate::knowledge::{KnowledgeError, ModelId, VectorSpace};

use super::StageError;
use super::StageOutcome;
use super::chunk::{self, CHUNK_BUDGET_TOKENS};

/// The embedder port (§9): pinned model identity, order-preserving batch embedding.
/// Sync by contract — CPU-bound batch; callers invoke it inside `spawn_blocking`
/// (§9).
pub trait Embedder: Send + Sync {
    /// Pinned model id — the quantization variant is part of the identity (§11.1).
    fn model_id(&self) -> &str;

    /// Embedding dimensionality (384 for the pinned model, §4).
    fn dim(&self) -> usize;

    /// Token count in **this model's tokenizer** (§4: chunk budgets are measured
    /// in the embedder's tokenizer, not characters or whitespace words). Includes
    /// the special tokens the model input carries, so the §8 Stage 3 budget
    /// (≤ 512) covers the full embedded string.
    fn count_tokens(&self, text: &str) -> usize;

    /// Embeds `texts`, preserving order and pairwise association.
    ///
    /// # Errors
    /// [`EmbedError`] — every impl maps runtime failures into this taxonomy.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

/// Embedding failures (§9).
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    /// The runtime or model is not loaded / not reachable.
    #[error("embedder unavailable: {0}")]
    Unavailable(String),
    /// Inference failed for the given batch.
    #[error("embedding inference failed: {0}")]
    Inference(String),
}

impl EmbedError {
    /// Retry class (§10): unavailability is transient; inference failures may be
    /// resource-shaped (OOM under contention) and retry via `max_attempts`.
    #[must_use]
    pub fn class(&self) -> Class {
        Class::Retry
    }
}

/// Maps a knowledge-plane failure into the stage taxonomy (§10): unavailability
/// is transient (contention, boot ordering), backend failures retry via
/// `max_attempts` — genuinely corrupt data surfaces as repeated failure, and the
/// audit trail carries the detail.
fn knowledge_err(err: KnowledgeError, attempt: u32) -> StageError {
    match err {
        KnowledgeError::Unavailable(detail) => StageError::Transient {
            source: Box::new(std::io::Error::other(detail)),
            class: Class::Retry,
            attempt,
        },
        backend @ KnowledgeError::Backend(_) => StageError::Transient {
            source: Box::new(std::io::Error::other(backend.to_string())),
            class: Class::Retry,
            attempt,
        },
    }
}

/// Maps an io failure on the system-of-record files (§3): a missing clean file
/// is permanent (the milestone claims it exists), anything else is transient.
fn io_err(err: std::io::Error, attempt: u32) -> StageError {
    if err.kind() == std::io::ErrorKind::NotFound {
        StageError::Permanent {
            reason: format!("clean file missing from data/: {err}"),
        }
    } else {
        StageError::transient(err, Class::Retry, attempt)
    }
}

fn attempt_of(job: &ClaimedJob) -> u32 {
    u32::try_from(job.attempts().saturating_add(1)).unwrap_or(u32::MAX)
}

/// The §3 chunk id: `sha256(doc_id:seq)` — immutable derived row, replay no-op.
fn chunk_id(doc_id: &str, seq: usize) -> String {
    crate::text::sha256_hex(&format!("{doc_id}:{seq}"))
}

/// Runs Stage 3 for one claimed job (§8): chunk → embed → registry rows +
/// vector upsert. Cross-store order per §7: registry rows commit first (the
/// intent), then vectors land, then the milestone advances in `complete`.
///
/// # Errors
/// [`StageError`] classified per §10.
pub(super) fn run(ctx: &super::StageCtx<'_>, job: &ClaimedJob) -> Result<StageOutcome, StageError> {
    let now = control::now();
    let attempt = attempt_of(job);
    let doc = control::get(ctx.conn, job.doc_id())?.ok_or_else(|| {
        StageError::permanent(format!(
            "document {} vanished mid-run (§10 invariant)",
            job.doc_id()
        ))
    })?;
    let Some(clean_path) = doc.clean_file_path.clone() else {
        return Err(StageError::permanent(format!(
            "document {} reached VECTORIZE without a clean file",
            job.doc_id()
        )));
    };
    let markdown = std::fs::read_to_string(&clean_path).map_err(|e| io_err(e, attempt))?;
    let title = doc.title.clone().unwrap_or_default();

    // §8 Stage 3: budget measured in the embedder's tokenizer, breadcrumb
    // included.
    let chunks = chunk::chunk_document(&markdown, &title, CHUNK_BUDGET_TOKENS, &|s| {
        ctx.embedder.count_tokens(s)
    });

    // §3 identity: chunk_id = sha256(doc_id:seq); §8 dedup: sha256(embed_text).
    let hashes: Vec<String> = chunks
        .iter()
        .map(|c| crate::text::sha256_hex(&c.embed_text))
        .collect();

    let model = ctx.embedder.model_id().to_string();
    let space = VectorSpace::Chunks {
        model_id: ModelId::new(model.clone()),
    };
    let existing = control::chunk_signatures(ctx.conn, job.doc_id())?;

    // Replay detection (§7.1): same seq→hash signature means the registry is
    // already true — repair missing vectors only. Any drift means re-chunk
    // (§7.4): delete-first, both stores.
    let identical = existing.len() == chunks.len()
        && existing
            .iter()
            .zip(&hashes)
            .enumerate()
            .all(|(i, (row, hash))| {
                row.seq == i64::try_from(i).unwrap_or(i64::MAX) && &row.content_hash == hash
            });

    if identical {
        // §7.3 repair path: only chunks whose vector is missing re-embed.
        let mut repair: Vec<&chunk::Chunk> = Vec::new();
        for (i, c) in chunks.iter().enumerate() {
            let id = chunk_id(job.doc_id(), i);
            let present = ctx
                .handle
                .block_on(ctx.knowledge.has_vector(space.clone(), &id))
                .map_err(|e| knowledge_err(e, attempt))?;
            if !present {
                repair.push(c);
            }
        }
        if repair.is_empty() {
            // Fully replayed: nothing to do anywhere (§7.1).
            return Ok(StageOutcome::Advance);
        }
        let (ids, vectors) = embed_unique(ctx, &repair, job.doc_id(), attempt)?;
        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        ctx.handle
            .block_on(
                ctx.knowledge
                    .upsert_vectors(space.clone(), job.doc_id(), &id_refs, &vectors),
            )
            .map_err(|e| knowledge_err(e, attempt))?;
        return Ok(StageOutcome::Advance);
    }

    // Fresh or changed content (§7.4): delete-first in the knowledge plane
    // (every chunk collection + graph edges), then replace the registry rows.
    ctx.handle
        .block_on(ctx.knowledge.delete_doc(job.doc_id()))
        .map_err(|e| knowledge_err(e, attempt))?;
    let rows: Vec<NewChunkRow> = chunks
        .iter()
        .enumerate()
        .map(|(i, c)| NewChunkRow {
            chunk_id: chunk_id(job.doc_id(), i),
            seq: i64::try_from(i).unwrap_or(i64::MAX),
            header_path: c.header_path.clone(),
            text: c.text.clone(),
            embed_text: c.embed_text.clone(),
            token_count: i64::try_from(ctx.embedder.count_tokens(&c.embed_text))
                .unwrap_or(i64::MAX),
            embedding_model: model.clone(),
            content_hash: hashes[i].clone(),
        })
        .collect();
    let token_total = rows.iter().map(|r| r.token_count).sum();
    control::replace_chunks(ctx.conn, job.doc_id(), &rows)?;
    let count = i64::try_from(rows.len()).unwrap_or(i64::MAX);
    control::update_vectorize_result(ctx.conn, job.doc_id(), count, token_total, &now)?;

    let chunk_refs: Vec<&chunk::Chunk> = chunks.iter().collect();
    let (ids, vectors) = embed_unique(ctx, &chunk_refs, job.doc_id(), attempt)?;
    let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    ctx.handle
        .block_on(
            ctx.knowledge
                .upsert_vectors(space.clone(), job.doc_id(), &id_refs, &vectors),
        )
        .map_err(|e| knowledge_err(e, attempt))?;
    Ok(StageOutcome::Advance)
}

/// Embeds `chunks`, computing each distinct `embed_text` exactly once (§8
/// Stage 3's exact-dup skip: repeated boilerplate sections share one inference)
/// and expanding the batch back out so every chunk id gets its vector.
fn embed_unique(
    ctx: &super::StageCtx<'_>,
    chunks: &[&chunk::Chunk],
    doc_id: &str,
    attempt: u32,
) -> Result<(Vec<String>, Vec<Vec<f32>>), StageError> {
    // Deduplicate by embed_text: first occurrence's index carries the vector.
    let mut uniques: Vec<&str> = Vec::new();
    let mut owner: Vec<usize> = Vec::with_capacity(chunks.len());
    for c in chunks {
        let idx = uniques.iter().position(|u| *u == c.embed_text.as_str());
        if let Some(i) = idx {
            owner.push(i);
        } else {
            owner.push(uniques.len());
            uniques.push(c.embed_text.as_str());
        }
    }
    let vectors = ctx.embedder.embed(&uniques).map_err(|e| {
        StageError::transient(std::io::Error::other(e.to_string()), e.class(), attempt)
    })?;
    if vectors.len() != uniques.len() {
        return Err(StageError::permanent(format!(
            "embedder returned {} vectors for {} texts (§9 order-preserving contract broken)",
            vectors.len(),
            uniques.len()
        )));
    }
    let ids: Vec<String> = (0..chunks.len()).map(|i| chunk_id(doc_id, i)).collect();
    // Expand: every chunk id pairs with its text's vector (duplicates share it).
    let expanded: Vec<Vec<f32>> = owner.iter().map(|&i| vectors[i].clone()).collect();
    Ok((ids, expanded))
}

/// The local ONNX embedder (§4): pinned `BAAI/bge-small-en-v1.5`, fp32, 384
/// dims, CLS pooling via `fastembed`. Model and tokenizer files are fetched
/// once from the HF hub into `cache_dir` (fail-fast at boot — §2's posture for
/// local runtimes) and reused offline afterwards.
#[cfg(feature = "onnx-embedder")]
pub struct LocalEmbedder {
    model: std::sync::Mutex<fastembed::TextEmbedding>,
    tokenizer: tokenizers::Tokenizer,
    model_id: String,
    dim: usize,
}

#[cfg(feature = "onnx-embedder")]
impl LocalEmbedder {
    /// The pinned model identity (§4): the fastembed variant and its HF repo,
    /// so the tokenizer comes from the same source as the ONNX graph.
    const MODEL: fastembed::EmbeddingModel = fastembed::EmbeddingModel::BGESmallENV15;
    const HF_REPO: &str = "Xenova/bge-small-en-v1.5";
    /// §4/§8: the budget the tokenizer must measure against — fastembed
    /// truncates at exactly this width.
    pub(crate) const MAX_LENGTH: usize = 512;

    /// Loads (downloading on first use) the pinned model into `cache_dir`.
    ///
    /// # Errors
    /// [`EmbedError::Unavailable`] when the hub is unreachable or the runtime
    /// cannot load the graph — a boot-time failure (§2 config fail-fast).
    pub fn new(cache_dir: &std::path::Path) -> Result<Self, EmbedError> {
        let unavailable = |e: String| EmbedError::Unavailable(e);
        std::fs::create_dir_all(cache_dir).map_err(|e| {
            unavailable(format!(
                "cannot create model cache {}: {e}",
                cache_dir.display()
            ))
        })?;
        let model = fastembed::TextEmbedding::try_new(
            fastembed::InitOptions::new(Self::MODEL)
                .with_cache_dir(cache_dir.to_path_buf())
                .with_max_length(Self::MAX_LENGTH)
                .with_show_download_progress(false),
        )
        .map_err(|e| {
            unavailable(format!(
                "pinned embedder {} failed to load: {e}",
                Self::MODEL
            ))
        })?;

        let info = fastembed::TextEmbedding::get_model_info(&Self::MODEL)
            .map_err(|e| unavailable(e.to_string()))?;
        let dim = info.dim;

        // The tokenizer from the same HF repo, loaded separately so the chunker
        // can budget in the embedder's tokenizer space (§4). fastembed shares
        // this cache layout, so the file is fetched at most once.
        let api = hf_hub::api::sync::ApiBuilder::new()
            .with_cache_dir(cache_dir.to_path_buf())
            .build()
            .map_err(|e| unavailable(format!("HF hub API init failed: {e}")))?;
        let tok_path = api
            .model(Self::HF_REPO.to_string())
            .get("tokenizer.json")
            .map_err(|e| unavailable(format!("tokenizer.json fetch failed: {e}")))?;
        let tokenizer = tokenizers::Tokenizer::from_file(&tok_path)
            .map_err(|e| unavailable(format!("tokenizer.json parse failed: {e}")))?;

        Ok(Self {
            model: std::sync::Mutex::new(model),
            tokenizer,
            model_id: "bge-small-en-v1.5".to_string(),
            dim,
        })
    }
}

#[cfg(feature = "onnx-embedder")]
impl Embedder for LocalEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn count_tokens(&self, text: &str) -> usize {
        // `encode` with special tokens: the model input is [CLS] text [SEP],
        // so the §8 budget counts them (fastembed encodes the same way).
        self.tokenizer.encode(text, true).map_or_else(
            |_| {
                // Unencodable input is a per-text concern, not a boot failure:
                // fall back to a whitespace floor so the chunker still budgets.
                text.split_whitespace().count() + 2
            },
            |enc| enc.get_ids().len(),
        )
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut model = self
            .model
            .lock()
            .map_err(|_| EmbedError::Unavailable("embedder mutex poisoned".to_string()))?;
        // Order-preserving batch (§9 contract): fastembed returns one embedding
        // per input, in order.
        model
            .embed(texts, None)
            .map_err(|e| EmbedError::Inference(e.to_string()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use tokio::runtime::Handle;

    use super::*;
    use crate::config::Config;
    use crate::control::testing::seed_doc;
    use crate::control::{self, ClaimedJob, Stage};
    use crate::knowledge::KnowledgeStore;
    use crate::pipeline::test_support::{
        FakeEmbedder, InMemoryKnowledge, NeverExtractor, NeverFetcher,
    };

    const NOW: &str = "2026-09-06 12:00:00";

    /// §8-flavored fixture: three headed sections with paragraphs and a fenced
    /// code block.
    const MARKDOWN: &str = "# On SQLite\n\nSQLite is an embedded database engine. \
        It runs inside the application process with zero configuration.\n\n\
        ## Storage\n\nThe store writes to a single file. WAL mode keeps readers \
        concurrent with one writer.\n\n```sql\nSELECT 1;\n```\n\n\
        ## Queries\n\nAnother paragraph about indexes and queries.";

    struct Fixture {
        #[allow(dead_code)] // keeps the tempdir alive for the test's duration
        dir: tempfile::TempDir,
        config: Arc<Config>,
        store: std::path::PathBuf,
        doc_id: String,
        clean_path: std::path::PathBuf,
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
        let clean_path = dir
            .path()
            .join("data")
            .join("clean")
            .join(format!("{doc_id}.md"));
        std::fs::create_dir_all(clean_path.parent().unwrap()).unwrap();
        std::fs::write(&clean_path, MARKDOWN).unwrap();
        conn.execute(
            "UPDATE documents SET status = 'CLEANED', clean_file_path = ?2, title = 'On SQLite' WHERE doc_id = ?1",
            rusqlite::params![doc_id, clean_path.to_string_lossy()],
        )
        .unwrap();
        drop(conn);
        Fixture {
            dir,
            config,
            store,
            doc_id,
            clean_path,
        }
    }

    fn vectorize_job(conn: &rusqlite::Connection, doc_id: &str) -> ClaimedJob {
        control::enqueue(conn, "v-job", doc_id, Stage::Vectorize, 5, None, NOW).unwrap();
        control::claim_next(conn, Stage::Vectorize, "w1", NOW, 60)
            .unwrap()
            .unwrap()
    }

    /// Runs the stage from a blocking thread with a connection whose lifetime
    /// is confined there — the production shape (the tick holds the connection
    /// in `spawn_blocking`).
    async fn run_stage(
        config: &Arc<Config>,
        store: &std::path::Path,
        job: &ClaimedJob,
        embedder: &Arc<FakeEmbedder>,
        knowledge: &Arc<InMemoryKnowledge>,
    ) -> Result<StageOutcome, StageError> {
        let config = Arc::clone(config);
        let store = store.to_path_buf();
        let job = job.clone();
        let embedder = Arc::clone(embedder);
        let knowledge = Arc::clone(knowledge);
        tokio::task::spawn_blocking(move || {
            let conn = control::connect(&store).unwrap();
            let handle = Handle::current();
            let ctx = super::super::StageCtx {
                config: config.as_ref(),
                conn: &conn,
                handle: &handle,
                fetcher: &NeverFetcher,
                extractor: &NeverExtractor,
                embedder: embedder.as_ref(),
                knowledge: knowledge.as_ref(),
                llm: &crate::pipeline::test_support::NeverLlm,
            };
            run(&ctx, &job)
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn chunks_registry_and_vectors_land_together() {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        let job = vectorize_job(&conn, &fx.doc_id);
        drop(conn);

        let embedder = Arc::new(FakeEmbedder::new());
        let knowledge = Arc::new(InMemoryKnowledge::default());
        let outcome = run_stage(&fx.config, &fx.store, &job, &embedder, &knowledge)
            .await
            .unwrap();
        assert_eq!(outcome, StageOutcome::Advance);

        let conn = control::connect(&fx.store).unwrap();
        let sigs = control::chunk_signatures(&conn, &fx.doc_id).unwrap();
        assert!(
            sigs.len() >= 3,
            "heading + paragraphs + code must chunk: {}",
            sigs.len()
        );
        let space = VectorSpace::Chunks {
            model_id: ModelId::new(embedder.model_id().to_string()),
        };
        for (i, sig) in sigs.iter().enumerate() {
            assert_eq!(sig.seq, i64::try_from(i).unwrap_or(i64::MAX));
            assert!(
                knowledge
                    .has_vector(space.clone(), &sig.chunk_id)
                    .await
                    .unwrap(),
                "chunk {i} vector missing"
            );
        }
        let doc = control::get(&conn, &fx.doc_id).unwrap().unwrap();
        assert_eq!(
            doc.chunk_count,
            i64::try_from(sigs.len()).unwrap_or(i64::MAX)
        );
        assert!(doc.token_count.unwrap_or(0) > 0);
        // The registry points at the embedder's model (§4).
        let stored_model: String = conn
            .query_row(
                "SELECT embedding_model FROM chunks WHERE doc_id = ?1 LIMIT 1",
                rusqlite::params![fx.doc_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored_model, embedder.model_id());
    }

    #[tokio::test]
    async fn replay_is_a_noop_and_missing_vectors_are_repaired() {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        let job = vectorize_job(&conn, &fx.doc_id);
        drop(conn);

        let embedder = Arc::new(FakeEmbedder::new());
        let knowledge = Arc::new(InMemoryKnowledge::default());
        run_stage(&fx.config, &fx.store, &job, &embedder, &knowledge)
            .await
            .unwrap();
        let calls_after_first = embedder.embed_calls();

        // Full replay: registry identical, every vector present → no embedding.
        run_stage(&fx.config, &fx.store, &job, &embedder, &knowledge)
            .await
            .unwrap();
        assert_eq!(
            embedder.embed_calls(),
            calls_after_first,
            "§7.1: replay must not re-embed"
        );

        // §7.3 repair: one vector lost → exactly that chunk re-embeds.
        let conn = control::connect(&fx.store).unwrap();
        let sigs = control::chunk_signatures(&conn, &fx.doc_id).unwrap();
        drop(conn);
        let lost = &sigs[0].chunk_id;
        knowledge.remove(lost);
        run_stage(&fx.config, &fx.store, &job, &embedder, &knowledge)
            .await
            .unwrap();
        assert_eq!(embedder.embed_calls(), calls_after_first + 1);
        let space = VectorSpace::Chunks {
            model_id: ModelId::new(embedder.model_id().to_string()),
        };
        assert!(knowledge.has_vector(space, lost).await.unwrap(), "repaired");
    }

    #[tokio::test]
    async fn changed_content_deletes_first_and_rechunks() {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        let job = vectorize_job(&conn, &fx.doc_id);
        drop(conn);

        let embedder = Arc::new(FakeEmbedder::new());
        let knowledge = Arc::new(InMemoryKnowledge::default());
        run_stage(&fx.config, &fx.store, &job, &embedder, &knowledge)
            .await
            .unwrap();
        let conn = control::connect(&fx.store).unwrap();
        let old = control::chunk_signatures(&conn, &fx.doc_id).unwrap();
        drop(conn);
        let space = VectorSpace::Chunks {
            model_id: ModelId::new(embedder.model_id().to_string()),
        };

        // Change the clean file (§7.4 re-chunk) and replay.
        let mut changed = MARKDOWN.to_string();
        changed.push_str("\n\n# Extra\n\nA brand new section that shifts every boundary.\n");
        std::fs::write(&fx.clean_path, changed).unwrap();
        run_stage(&fx.config, &fx.store, &job, &embedder, &knowledge)
            .await
            .unwrap();

        let conn = control::connect(&fx.store).unwrap();
        let fresh = control::chunk_signatures(&conn, &fx.doc_id).unwrap();
        drop(conn);
        assert!(
            fresh.len() > old.len(),
            "re-chunk must replace rows: {} -> {}",
            old.len(),
            fresh.len()
        );
        let fresh_ids: std::collections::HashSet<&str> =
            fresh.iter().map(|s| s.chunk_id.as_str()).collect();
        for old_sig in &old {
            if fresh_ids.contains(old_sig.chunk_id.as_str()) {
                continue; // still part of the new chunking — legitimately kept
            }
            assert!(
                !knowledge
                    .has_vector(space.clone(), &old_sig.chunk_id)
                    .await
                    .unwrap(),
                "stale vector for {} must be gone (§7.4 delete-first)",
                old_sig.chunk_id
            );
        }
        for sig in &fresh {
            assert!(
                knowledge
                    .has_vector(space.clone(), &sig.chunk_id)
                    .await
                    .unwrap()
            );
        }
    }

    #[tokio::test]
    async fn missing_clean_file_is_permanent() {
        let fx = fixture();
        std::fs::remove_file(&fx.clean_path).unwrap();
        let conn = control::connect(&fx.store).unwrap();
        let job = vectorize_job(&conn, &fx.doc_id);
        drop(conn);

        let embedder = Arc::new(FakeEmbedder::new());
        let knowledge = Arc::new(InMemoryKnowledge::default());
        let err = run_stage(&fx.config, &fx.store, &job, &embedder, &knowledge)
            .await
            .unwrap_err();
        assert!(matches!(err, StageError::Permanent { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn exact_duplicates_embed_once() {
        // Two sections with identical body text: one inference, two rows.
        let fx = fixture();
        let md =
            "# A\n\nRepeated boilerplate paragraph.\n\n# B\n\nRepeated boilerplate paragraph.\n";
        std::fs::write(&fx.clean_path, md).unwrap();
        let conn = control::connect(&fx.store).unwrap();
        let job = vectorize_job(&conn, &fx.doc_id);
        drop(conn);

        let embedder = Arc::new(FakeEmbedder::new());
        let knowledge = Arc::new(InMemoryKnowledge::default());
        run_stage(&fx.config, &fx.store, &job, &embedder, &knowledge)
            .await
            .unwrap();
        assert_eq!(
            embedder.embed_calls(),
            1,
            "§8 exact-dup skip: one inference"
        );
        let conn = control::connect(&fx.store).unwrap();
        let sigs = control::chunk_signatures(&conn, &fx.doc_id).unwrap();
        drop(conn);
        assert_eq!(sigs.len(), 2, "both rows stored");
    }

    #[cfg(feature = "onnx-embedder")]
    #[test]
    #[ignore = "fetches the pinned model from the HF hub on first run; run with --ignored on a networked machine"]
    fn local_embedder_loads_pinned_model_and_counts_tokens() {
        // Exercises the real stack: hub fetch, ONNX session, tokenizer. The
        // cache persists under ~/.cache/ohara-test so the ~130 MB model is
        // fetched once across runs (networked DNS is flaky mid-download).
        let base = match std::env::var_os("HOME") {
            Some(h) => {
                let dir = std::path::PathBuf::from(h).join(".cache/ohara-test");
                std::fs::create_dir_all(&dir).expect("create cache dir");
                dir
            }
            None => tempfile::tempdir().expect("tempdir").path().to_path_buf(),
        };
        let embedder = LocalEmbedder::new(&base.join("models"))
            .unwrap_or_else(|e| panic!("local embedder unavailable: {e}"));
        assert_eq!(embedder.model_id(), "bge-small-en-v1.5");
        assert_eq!(embedder.dim(), 384);
        let toks = embedder.count_tokens("The quick brown fox jumps over the lazy dog.");
        assert!(toks > 8, "specials + subwords must count: {toks}");
        let vecs = embedder
            .embed(&["SQLite is an embedded database."])
            .unwrap();
        assert_eq!(vecs[0].len(), 384);
        let norm: f32 = vecs[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-2,
            "bge embeddings are L2-normalized: {norm}"
        );
    }
}
