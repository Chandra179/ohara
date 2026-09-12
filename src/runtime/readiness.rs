//! Readiness probing for the default local runtime.
//!
//! The checks construct concrete adapters here, then return provider-neutral
//! diagnostics to the HTTP transport. This keeps operational detail out of
//! the transport boundary while preserving actionable local errors.

use crate::config::Config;
use crate::engine::Ollama;

/// One component's readiness result.
#[derive(Debug, Clone)]
pub(crate) struct ReadinessCheck {
    /// Whether the component can serve its configured role.
    pub(crate) available: bool,
    /// Actionable detail when the component is unavailable.
    pub(crate) diagnostic: Option<ReadinessDiagnostic>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReadinessDiagnostic {
    /// Stable component identifier used by the transport contract.
    pub(crate) component: &'static str,
    /// Human-readable failure detail.
    pub(crate) message: String,
    /// Suggested recovery action.
    pub(crate) action: String,
}

/// Readiness report for the default local runtime.
#[derive(Debug, Clone)]
pub(crate) struct ReadinessReport {
    /// `SQLite` control store status.
    pub(crate) control_store: ReadinessCheck,
    /// `LadybugDB` knowledge store status.
    pub(crate) knowledge_store: ReadinessCheck,
    /// Local embedding model status.
    pub(crate) embedder: ReadinessCheck,
    /// Configured language-model endpoint status.
    pub(crate) llm: ReadinessCheck,
}

/// Checks every default runtime dependency and returns all failures together.
pub(crate) async fn readiness(config: Config) -> ReadinessReport {
    let (control_store, knowledge_store, embedder, llm) = tokio::join!(
        check_control_store(config.clone()),
        check_knowledge_store(config.clone()),
        check_embedder(config.clone()),
        check_llm(config),
    );
    ReadinessReport {
        control_store,
        knowledge_store,
        embedder,
        llm,
    }
}

fn available() -> ReadinessCheck {
    ReadinessCheck {
        available: true,
        diagnostic: None,
    }
}

fn unavailable(
    component: &'static str,
    message: impl Into<String>,
    action: impl Into<String>,
) -> ReadinessCheck {
    ReadinessCheck {
        available: false,
        diagnostic: Some(ReadinessDiagnostic {
            component,
            message: message.into(),
            action: action.into(),
        }),
    }
}

async fn check_control_store(config: Config) -> ReadinessCheck {
    let result =
        tokio::task::spawn_blocking(move || crate::control::connect(config.db_path()).map(|_| ()))
            .await;
    match result {
        Ok(Ok(())) => available(),
        Ok(Err(error)) => unavailable(
            "controlStore",
            format!("control store is unavailable: {error}"),
            "Check the configured database path and permissions.",
        ),
        Err(error) => unavailable(
            "controlStore",
            format!("control store health check failed: {error}"),
            "Restart the local API and check the process logs.",
        ),
    }
}

#[cfg(feature = "ladybug")]
async fn check_knowledge_store(config: Config) -> ReadinessCheck {
    let result = tokio::task::spawn_blocking(move || {
        crate::knowledge::LadybugStore::open(
            &config.data_dir().join("ladybug"),
            config.embedder().dim(),
        )
        .map(|_| ())
    })
    .await;
    match result {
        Ok(Ok(())) => available(),
        Ok(Err(error)) => unavailable(
            "knowledgeStore",
            format!("knowledge store is unavailable: {error}"),
            "Repair or rebuild the local knowledge index, then restart Ohara.",
        ),
        Err(error) => unavailable(
            "knowledgeStore",
            format!("knowledge store health check failed: {error}"),
            "Restart the local API and check the knowledge-store logs.",
        ),
    }
}

#[cfg(not(feature = "ladybug"))]
fn check_knowledge_store(_config: Config) -> std::future::Ready<ReadinessCheck> {
    std::future::ready(unavailable(
        "knowledgeStore",
        "Ohara was built without the `ladybug` feature.",
        "Use the default feature set or inject a KnowledgeStore.",
    ))
}

async fn check_embedder(config: Config) -> ReadinessCheck {
    let result =
        tokio::task::spawn_blocking(move || crate::pipeline::check_embedder_readiness(&config))
            .await;
    match result {
        Ok(Ok(())) => available(),
        Ok(Err(error)) => unavailable(
            "embedder",
            error.to_string(),
            "Run the worker once while online to download the pinned embedding model, then retry.",
        ),
        Err(error) => unavailable(
            "embedder",
            format!("embedder health check failed: {error}"),
            "Restart the local API and check the model-cache permissions.",
        ),
    }
}

async fn check_llm(config: Config) -> ReadinessCheck {
    let Ok(provider) = Ollama::new(
        config.llm().base_url().clone(),
        config.llm().health_timeout(),
    ) else {
        return unavailable(
            "llm",
            "the configured local language-model client could not be created",
            "Check the configured LLM URL and restart the local API.",
        );
    };
    match provider.verify_endpoint().await {
        Ok(()) => available(),
        Err(error) => unavailable(
            "llm",
            format!("local language-model endpoint is unavailable: {error}"),
            "Start the configured local provider and retry.",
        ),
    }
}
