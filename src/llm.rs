//! The single LLM port (§9, §1.3): provider implementations expose one stable
//! completion interface, while outbound networking belongs to the Engine and
//! usage recording stays at the pipeline boundary. Local Ollama is the
//! default; cloud is opt-in (§12).

use async_trait::async_trait;

use crate::Class;

/// One completion request (§9: schema-validated JSON where the provider supports
/// it, temperature-0 determinism for extraction).
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// Provider model id — pinned per §2 (e.g. `gemma3:1b`).
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

/// The LLM port (§9): completion with provider identity. Durable usage accounting
/// is recorded by the pipeline usage-ledger Module so this plane stays free of
/// control-store dependencies.
#[async_trait]
pub trait Llm: Send + Sync {
    /// Stable provider name stored with each completion attempt.
    fn provider_name(&self) -> &str;

    /// Completes `req`.
    ///
    /// # Errors
    /// [`LlmError`] per its taxonomy and retry classes.
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError>;
}

/// An inert [`Llm`] for deployments that never call one (§6: with
/// `graph_enabled = false` the chain ends at VECTORIZE and no stage reaches the
/// LLM). Every completion fails fast with a named reason instead of silently
/// returning nothing.
pub struct NoLlm;

#[async_trait]
impl Llm for NoLlm {
    fn provider_name(&self) -> &'static str {
        "none"
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let _ = req;
        Err(LlmError::Unavailable(
            "no LLM provider is wired (graph_enabled = false, §6)".to_string(),
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn error_classes_follow_the_taxonomy() {
        assert_eq!(
            LlmError::Unavailable("down".to_string()).class(),
            Class::Retry
        );
        assert_eq!(LlmError::RateLimited.class(), Class::Retry);
    }

    #[tokio::test]
    async fn no_llm_never_completes() {
        let err = NoLlm
            .complete(CompletionRequest {
                model: "gemma3:1b".to_string(),
                prompt: "x".to_string(),
                max_tokens: None,
                temperature: 0.0,
                json_schema: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Unavailable(_)), "got {err:?}");
        assert_eq!(NoLlm.provider_name(), "none");
    }
}
