//! Local Ollama adapter for the [`crate::llm::Llm`] port.
//!
//! The LLM port remains provider-neutral at the crate root. This adapter lives
//! in the Engine plane because it owns the outbound HTTP connection to the
//! local Ollama endpoint; callers depend only on the port.

use async_trait::async_trait;

use crate::llm::{CompletionRequest, CompletionResponse, Llm, LlmError};

/// The local Ollama provider (§2): an OpenAI-compatible endpoint the user runs
/// themselves (`http://localhost:11434` by default) — nothing leaves the machine
/// (§12). Structured outputs ride the `OpenAI` `response_format` JSON-schema
/// mechanism, which Ollama supports; extraction therefore gets schema-validated
/// JSON instead of best-effort prose.
pub struct Ollama {
    base: url::Url,
    client: reqwest::Client,
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
                .and_then(|usage| usage.prompt_tokens)
                .unwrap_or(0),
            completion_tokens: parsed
                .usage
                .as_ref()
                .and_then(|usage| usage.completion_tokens)
                .unwrap_or(0),
        })
    }
}

#[async_trait]
impl Llm for Ollama {
    fn provider_name(&self) -> &'static str {
        "ollama"
    }

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
            200 => Self::parse_chat(&bytes),
            429 => Err(LlmError::RateLimited),
            404 => Err(LlmError::Unavailable(format!(
                "model {:?} not found on the endpoint",
                req.model
            ))),
            code => Err(LlmError::Unavailable(format!(
                "completion endpoint answered HTTP {code}"
            ))),
        }
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
        assert_eq!(err.class(), crate::Class::Permanent);

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
}
