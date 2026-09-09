//! The single LLM port (§9, §1.3): provider impls behind one trait, so cost
//! accounting and the data-governance decision (§12 egress) live in exactly one
//! place. Local Ollama is the default; cloud is opt-in (§12).

use std::sync::Mutex;

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

/// The local Ollama provider (§2): an OpenAI-compatible endpoint the user runs
/// themselves (`http://localhost:11434` by default) — nothing leaves the machine
/// (§12). Structured outputs ride the `OpenAI` `response_format` JSON-schema
/// mechanism, which Ollama supports; extraction therefore gets schema-validated
/// JSON instead of best-effort prose.
pub struct Ollama {
    base: url::Url,
    client: reqwest::Client,
    usage: Mutex<LlmUsage>,
    health_timeout: std::time::Duration,
}

/// The OpenAI-compatible completions path on an Ollama endpoint.
const COMPLETIONS_PATH: &str = "/v1/chat/completions";

impl Ollama {
    /// Builds the client for `base_url` (config-validated http/https). No network
    /// I/O happens here — [`Ollama::verify_endpoint`] is the §2 boot health gate.
    ///
    /// # Errors
    /// [`LlmError::Unavailable`] if the HTTP client cannot be built.
    pub fn new(base_url: url::Url, health_timeout: std::time::Duration) -> Result<Self, LlmError> {
        let client = reqwest::Client::builder().build().map_err(|e| {
            LlmError::Unavailable(format!("cannot build http client for {base_url}: {e}"))
        })?;
        Ok(Self {
            base: base_url,
            client,
            usage: Mutex::new(LlmUsage::default()),
            health_timeout,
        })
    }

    /// §2 boot health gate: the endpoint must answer `GET /api/tags` (the native
    /// Ollama liveness probe — model-independent, so a missing pinned model is a
    /// different, later failure). Fail-fast at boot, never mid-stage.
    ///
    /// # Errors
    /// [`LlmError::Unavailable`] when the probe fails; [`LlmError::RateLimited`]
    /// when the endpoint throttles even the probe.
    pub async fn verify_endpoint(&self) -> Result<(), LlmError> {
        let url = self
            .base
            .join("/api/tags")
            .map_err(|e| LlmError::Unavailable(format!("base_url join failed: {e}")))?;
        let resp = self
            .client
            .get(url)
            .timeout(self.health_timeout)
            .send()
            .await
            .map_err(|e| LlmError::Unavailable(format!("ollama health probe failed: {e}")))?;
        match resp.status().as_u16() {
            200 => Ok(()),
            429 => Err(LlmError::RateLimited),
            code => Err(LlmError::Unavailable(format!(
                "ollama health probe got HTTP {code}"
            ))),
        }
    }

    /// Adds one call's token counts to the cumulative usage (a failed call still
    /// consumed egress, §12).
    fn record_usage(&self, prompt: u64, completion: u64) {
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        usage.calls += 1;
        usage.prompt_tokens += prompt;
        usage.completion_tokens += completion;
    }
}

/// The request body sent to the OpenAI-compatible completions endpoint. Kept as
/// JSON values so `json_schema` passes through verbatim (the port owns the
/// structured-outputs mechanism, §9 rule 3 — stage code never names it).
#[derive(serde::Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat<'a>>,
}

#[derive(serde::Serialize)]
struct Message<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(serde::Serialize)]
struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    json_schema: JsonSchema<'a>,
}

#[derive(serde::Serialize)]
struct JsonSchema<'a> {
    name: &'static str,
    schema: &'a serde_json::Value,
    strict: bool,
}

/// The completion response's message/usage slice (`OpenAI` shape, as served by
/// Ollama's compatibility layer).
#[derive(serde::Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(serde::Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(serde::Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

#[derive(serde::Deserialize)]
struct Usage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

impl Ollama {
    /// Parses one completion response body. A missing choice/content is an
    /// [`LlmError::InvalidResponse`] — temperature-0 regeneration would reproduce
    /// it, so it is not retried (§10).
    fn parse_chat(body: &[u8]) -> Result<CompletionResponse, LlmError> {
        let parsed: ChatResponse = serde_json::from_slice(body).map_err(|e| {
            LlmError::InvalidResponse(format!("completion response is not valid JSON: {e}"))
        })?;
        let choice = parsed.choices.first().ok_or_else(|| {
            LlmError::InvalidResponse("completion response has no choices".to_string())
        })?;
        let text = choice.message.content.clone().ok_or_else(|| {
            LlmError::InvalidResponse("completion response has no content".to_string())
        })?;
        Ok(CompletionResponse {
            text,
            prompt_tokens: parsed
                .usage
                .as_ref()
                .and_then(|u| u.prompt_tokens)
                .unwrap_or(0),
            completion_tokens: parsed
                .usage
                .as_ref()
                .and_then(|u| u.completion_tokens)
                .unwrap_or(0),
        })
    }
}

#[async_trait]
impl Llm for Ollama {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let url = self
            .base
            .join(COMPLETIONS_PATH)
            .map_err(|e| LlmError::Unavailable(format!("base_url join failed: {e}")))?;
        let response_format = req.json_schema.as_ref().map(|schema| ResponseFormat {
            kind: "json_schema",
            json_schema: JsonSchema {
                name: "response",
                schema,
                strict: true,
            },
        });
        let body = ChatRequest {
            model: &req.model,
            messages: vec![Message {
                role: "user",
                content: &req.prompt,
            }],
            temperature: req.temperature,
            max_tokens: req.max_tokens,
            response_format,
        };
        let resp = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Unavailable(format!("completion request failed: {e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| LlmError::Unavailable(format!("reading completion response: {e}")))?;
        match status {
            200 => {
                let parsed = Self::parse_chat(&bytes)?;
                self.record_usage(parsed.prompt_tokens, parsed.completion_tokens);
                Ok(parsed)
            }
            429 => {
                self.record_usage(0, 0);
                Err(LlmError::RateLimited)
            }
            404 => {
                self.record_usage(0, 0);
                Err(LlmError::Unavailable(format!(
                    "model {:?} not found on the endpoint",
                    req.model
                )))
            }
            code => {
                self.record_usage(0, 0);
                Err(LlmError::Unavailable(format!(
                    "completion endpoint answered HTTP {code}"
                )))
            }
        }
    }

    fn usage(&self) -> LlmUsage {
        *self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// An inert [`Llm`] for deployments that never call one (§6: with
/// `graph_enabled = false` the chain ends at VECTORIZE and no stage reaches the
/// LLM). Every completion fails fast with a named reason instead of silently
/// returning nothing.
pub struct NoLlm;

#[async_trait]
impl Llm for NoLlm {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let _ = req;
        Err(LlmError::Unavailable(
            "no LLM provider is wired (graph_enabled = false, §6)".to_string(),
        ))
    }

    fn usage(&self) -> LlmUsage {
        LlmUsage::default()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_wellformed_completion_response() {
        let body = r#"{
            "choices": [{"message": {"role": "assistant", "content": "{\"ok\": true}"}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3}
        }"#;
        let parsed = Ollama::parse_chat(body.as_bytes()).unwrap();
        assert_eq!(parsed.text, "{\"ok\": true}");
        assert_eq!(parsed.prompt_tokens, 12);
        assert_eq!(parsed.completion_tokens, 3);
    }

    #[test]
    fn maps_malformed_responses_to_invalid_response() {
        let err = Ollama::parse_chat(b"not json").unwrap_err();
        assert!(matches!(err, LlmError::InvalidResponse(_)), "got {err:?}");
        assert_eq!(err.class(), Class::Permanent);

        let no_choices = r#"{"choices": [], "usage": null}"#;
        let err = Ollama::parse_chat(no_choices.as_bytes()).unwrap_err();
        assert!(matches!(err, LlmError::InvalidResponse(_)), "got {err:?}");

        let no_content = r#"{"choices": [{"message": {}}], "usage": null}"#;
        let err = Ollama::parse_chat(no_content.as_bytes()).unwrap_err();
        assert!(matches!(err, LlmError::InvalidResponse(_)), "got {err:?}");
    }

    #[test]
    fn missing_usage_counts_zero() {
        let body = r#"{"choices": [{"message": {"content": "hi"}}], "usage": null}"#;
        let parsed = Ollama::parse_chat(body.as_bytes()).unwrap();
        assert_eq!((parsed.prompt_tokens, parsed.completion_tokens), (0, 0));
    }

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
                model: "phi4-mini:latest".to_string(),
                prompt: "x".to_string(),
                max_tokens: None,
                temperature: 0.0,
                json_schema: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Unavailable(_)), "got {err:?}");
        assert_eq!(NoLlm.usage(), LlmUsage::default());
    }
}
