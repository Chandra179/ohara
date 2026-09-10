//! Durable LLM completion-attempt recording.
//!
//! Provider implementations stay in the LLM plane and know nothing about
//! `SQLite`. This Module joins a provider call to the control-plane Usage Ledger,
//! recording both successful and failed attempts before returning the result.

use crate::config::Config;
use crate::control::{self, ControlDb, LlmUsageEvent, LlmUsageOutcome};
use crate::llm::{CompletionRequest, CompletionResponse, Llm, LlmError};

/// Context attached to one Completion Attempt.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CompletionContext<'a> {
    /// Operation that initiated the call.
    pub(crate) operation: &'a str,
    /// Document associated with extraction, if any.
    pub(crate) doc_id: Option<&'a str>,
    /// Job associated with extraction, if any.
    pub(crate) job_id: Option<&'a str>,
}

/// Failure while completing or recording one LLM attempt.
#[derive(Debug, thiserror::Error)]
pub(crate) enum UsageError {
    /// The provider call failed.
    #[error("llm provider: {0}")]
    Provider(#[source] LlmError),
    /// The provider result could not be appended to the Usage Ledger.
    #[error("llm usage ledger: {0}")]
    Ledger(#[source] crate::control::DbError),
}

/// Completes one request and appends exactly one durable Usage Ledger row.
pub(crate) async fn complete(
    config: &Config,
    conn: &ControlDb,
    llm: &dyn Llm,
    request: CompletionRequest,
    context: CompletionContext<'_>,
) -> Result<CompletionResponse, UsageError> {
    let model = request.model.clone();
    let result = llm.complete(request).await;
    let (outcome, prompt_tokens, completion_tokens, error) = match &result {
        Ok(response) => (
            LlmUsageOutcome::Succeeded,
            response.prompt_tokens,
            response.completion_tokens,
            None,
        ),
        Err(error) => (LlmUsageOutcome::Failed, 0, 0, Some(error.to_string())),
    };
    let estimated_cost_micros = estimate_cost(config, prompt_tokens, completion_tokens);
    let event = LlmUsageEvent {
        operation: context.operation,
        doc_id: context.doc_id,
        job_id: context.job_id,
        provider: llm.provider_name(),
        model: &model,
        outcome,
        prompt_tokens,
        completion_tokens,
        estimated_cost_micros,
        error: error.as_deref(),
    };
    control::record_llm_usage(conn, &event).map_err(UsageError::Ledger)?;
    result.map_err(UsageError::Provider)
}

fn estimate_cost(config: &Config, prompt_tokens: u64, completion_tokens: u64) -> u64 {
    let input = u128::from(prompt_tokens).saturating_mul(u128::from(
        config.llm().input_cost_micros_per_million_tokens(),
    )) / 1_000_000;
    let output = u128::from(completion_tokens).saturating_mul(u128::from(
        config.llm().output_cost_micros_per_million_tokens(),
    )) / 1_000_000;
    u64::try_from(input.saturating_add(output)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use async_trait::async_trait;

    use super::{CompletionContext, UsageError, complete};
    use crate::config::Config;
    use crate::control;
    use crate::llm::{CompletionRequest, CompletionResponse, Llm, LlmError};

    struct FakeLlm {
        result: Result<String, LlmError>,
    }

    #[async_trait]
    impl Llm for FakeLlm {
        fn provider_name(&self) -> &'static str {
            "fake"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, LlmError> {
            match &self.result {
                Ok(text) => Ok(CompletionResponse {
                    text: text.clone(),
                    prompt_tokens: 100,
                    completion_tokens: 25,
                }),
                Err(error) => Err(match error {
                    LlmError::Unavailable(message) => LlmError::Unavailable(message.clone()),
                    LlmError::RateLimited => LlmError::RateLimited,
                    LlmError::InvalidResponse(message) => {
                        LlmError::InvalidResponse(message.clone())
                    }
                }),
            }
        }
    }

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "test-model".to_string(),
            prompt: "test".to_string(),
            max_tokens: None,
            temperature: 0.0,
            json_schema: None,
        }
    }

    #[tokio::test]
    async fn records_success_and_cost_with_context() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("ohara.toml");
        std::fs::write(
            &path,
            "[llm]\ninput_cost_micros_per_million_tokens = 2000000\noutput_cost_micros_per_million_tokens = 4000000\n",
        )
        .expect("config");
        let config = Config::load(Some(&path)).expect("config");
        let db = control::testing::boot();
        let response = complete(
            &config,
            &db,
            &FakeLlm {
                result: Ok("done".to_string()),
            },
            request(),
            CompletionContext {
                operation: "extract",
                doc_id: Some("doc-1"),
                job_id: Some("job-1"),
            },
        )
        .await
        .expect("completion");
        assert_eq!(response.prompt_tokens, 100);
        let usage = control::metrics(&db, "2026-09-10 00:00:00")
            .expect("metrics")
            .llm_usage;
        assert_eq!(usage.calls, 1);
        assert_eq!(usage.successful_calls, 1);
        assert_eq!(usage.failed_calls, 0);
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 25);
        assert_eq!(usage.estimated_cost_micros, 300);
        let row: (String, String, String) = db
            .raw()
            .query_row(
                "SELECT operation, provider, doc_id FROM llm_usage",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("ledger row");
        assert_eq!(
            row,
            (
                "extract".to_string(),
                "fake".to_string(),
                "doc-1".to_string()
            )
        );
    }

    #[tokio::test]
    async fn records_failed_attempt_before_returning_provider_error() {
        let config = Config::load(None).expect("defaults");
        let db = control::testing::boot();
        let error = complete(
            &config,
            &db,
            &FakeLlm {
                result: Err(LlmError::RateLimited),
            },
            request(),
            CompletionContext {
                operation: "synthesis",
                doc_id: None,
                job_id: None,
            },
        )
        .await
        .expect_err("provider error");
        assert!(matches!(error, UsageError::Provider(LlmError::RateLimited)));
        let usage = control::metrics(&db, "2026-09-10 00:00:00")
            .expect("metrics")
            .llm_usage;
        assert_eq!(
            (usage.calls, usage.successful_calls, usage.failed_calls),
            (1, 0, 1)
        );
    }
}
