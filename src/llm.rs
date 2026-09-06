//! The single LLM port (§9, §1.3): provider impls behind one trait, so cost
//! accounting and the data-governance decision (§12 egress) live in exactly one
//! place. Local Ollama is the default; cloud is opt-in (§12).

use async_trait::async_trait;

use crate::Class;

/// One completion request (§9: schema-validated JSON where the provider supports
/// it, temperature-0 determinism for extraction).
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// Provider model id — pinned per §2 (e.g. `phi4-mini:latest`).
    pub model: String,
    /// The rendered prompt.
    pub prompt: String,
    /// Generation token limit, if any.
    pub max_tokens: Option<u32>,
    /// Sampling temperature — 0 for extraction (§9).
    pub temperature: f32,
    /// JSON schema the output must satisfy (structured outputs), when supported.
    pub json_schema: Option<serde_json::Value>,
}

/// One completion response, with the usage counters feeding the cost model (§11).
#[derive(Debug, Clone)]
pub struct CompletionResponse {
    /// The completion text.
    pub text: String,
    /// Prompt tokens consumed.
    pub prompt_tokens: u64,
    /// Completion tokens produced.
    pub completion_tokens: u64,
}

/// Cumulative token/call counters — the cost-model input (§11) and the egress
/// audit (§12).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LlmUsage {
    /// Completed calls.
    pub calls: u64,
    /// Sum of prompt tokens.
    pub prompt_tokens: u64,
    /// Sum of completion tokens.
    pub completion_tokens: u64,
}

/// LLM failures (§2, §9). Mid-run endpoint unavailability is Transient (backoff) —
/// health is checked at boot, but can lapse mid-run (§2).
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// The endpoint is unreachable or not ready.
    #[error("llm endpoint unavailable: {0}")]
    Unavailable(String),
    /// The endpoint throttled the request.
    #[error("llm rate limited")]
    RateLimited,
    /// The response did not satisfy the request's schema/shape.
    #[error("llm response invalid: {0}")]
    InvalidResponse(String),
}

impl LlmError {
    /// Retry class (§10): unavailability and throttling retry; an invalid response
    /// is permanent — temperature-0 regeneration is deterministic, so a blind retry
    /// cannot succeed (the stage layer decides whether that means DEAD or a
    /// prompt/model revision, §8 Stage 4).
    #[must_use]
    pub fn class(&self) -> Class {
        match self {
            LlmError::Unavailable(_) | LlmError::RateLimited => Class::Retry,
            LlmError::InvalidResponse(_) => Class::Permanent,
        }
    }
}

/// The LLM port (§9): completion with usage accounting. Cost counters advance on
/// every call, success or failure — a failed call still consumed egress.
#[async_trait]
pub trait Llm: Send + Sync {
    /// Completes `req`.
    ///
    /// # Errors
    /// [`LlmError`] per its taxonomy and retry classes.
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError>;

    /// Cumulative usage counters.
    fn usage(&self) -> LlmUsage;
}
