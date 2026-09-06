//! Stage 3b — Embed, and the [`Embedder`] port (§1.3, §9). The ONNX implementation
//! lands with the §15 step 4 build step, after the `LadybugDB` build gates pass.

use crate::Class;

/// The embedder port (§9): pinned model identity, order-preserving batch embedding.
/// Sync by contract — CPU-bound batch; callers invoke it inside `spawn_blocking`
/// (§9).
pub trait Embedder: Send + Sync {
    /// Pinned model id — the quantization variant is part of the identity (§11.1).
    fn model_id(&self) -> &str;

    /// Embedding dimensionality (384 for the pinned model, §4).
    fn dim(&self) -> usize;

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
