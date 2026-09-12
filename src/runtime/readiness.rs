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
    /// Qdrant and `FalkorDB` knowledge-service status.
    pub(crate) knowledge_store: ReadinessCheck,
    /// Local embedding model status.
    pub(crate) embedder: ReadinessCheck,
    /// Configured language-model endpoint status.
    pub(crate) llm: ReadinessCheck,
    /// Durable worker lifecycle and heartbeat status.
    pub(crate) worker: WorkerReadiness,
}

/// Worker-specific readiness projection used by the local API.
#[derive(Debug, Clone)]
pub(crate) struct WorkerReadiness {
    /// Whether a live worker is ready or executing a stage.
    pub(crate) available: bool,
    /// Most recently observed worker, if one has registered.
    pub(crate) observation: Option<crate::control::WorkerObservation>,
    /// Actionable diagnostic when ingestion is unavailable.
    pub(crate) diagnostic: Option<ReadinessDiagnostic>,
}

/// Checks every default runtime dependency and returns all failures together.
pub(crate) async fn readiness(config: Config) -> ReadinessReport {
    let (control_store, knowledge_store, embedder, llm) = tokio::join!(
        check_control_store(config.clone()),
        check_knowledge_store(config.clone()),
        check_embedder(config.clone()),
        check_llm(config.clone()),
    );
    let worker = if control_store.available {
        check_worker(config).await
    } else {
        WorkerReadiness {
            available: false,
            observation: None,
            diagnostic: None,
        }
    };
    ReadinessReport {
        control_store,
        knowledge_store,
        embedder,
        llm,
        worker,
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
        diagnostic: Some(diagnostic(component, message, action)),
    }
}

fn diagnostic(
    component: &'static str,
    message: impl Into<String>,
    action: impl Into<String>,
) -> ReadinessDiagnostic {
    ReadinessDiagnostic {
        component,
        message: message.into(),
        action: action.into(),
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

async fn check_knowledge_store(config: Config) -> ReadinessCheck {
    let result = match crate::knowledge::RemoteKnowledgeStore::connect(
        config.knowledge(),
        config.embedder().dim(),
        true,
    ) {
        Ok(store) => store.health().await,
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => available(),
        Err(error) => unavailable(
            "knowledgeStore",
            format!("knowledge store is unavailable: {error}"),
            "Run `make services`, then check the Qdrant and FalkorDB logs.",
        ),
    }
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

async fn check_worker(config: Config) -> WorkerReadiness {
    let now = crate::control::now();
    let stale_after = config.lease_ttl();
    let result = tokio::task::spawn_blocking(move || {
        let db = crate::control::connect(config.db_path())?;
        crate::control::latest_worker_status(&db, &now, stale_after)
    })
    .await;

    match result {
        Ok(Ok(Some(observation))) => {
            let available = !observation.stale
                && matches!(
                    observation.status.state,
                    crate::control::WorkerState::Ready | crate::control::WorkerState::Running
                );
            let diagnostic = (!available).then(|| worker_diagnostic(&observation));
            WorkerReadiness {
                available,
                observation: Some(observation),
                diagnostic,
            }
        }
        Ok(Ok(None)) => WorkerReadiness {
            available: false,
            observation: None,
            diagnostic: Some(diagnostic(
                "worker",
                "no worker process has registered with the control store",
                "Start the Ohara worker to enable ingestion.",
            )),
        },
        Ok(Err(error)) => WorkerReadiness {
            available: false,
            observation: None,
            diagnostic: Some(diagnostic(
                "worker",
                format!("worker status is unavailable: {error}"),
                "Check the control-store path and worker logs.",
            )),
        },
        Err(error) => WorkerReadiness {
            available: false,
            observation: None,
            diagnostic: Some(diagnostic(
                "worker",
                format!("worker health check failed: {error}"),
                "Restart the local API and check the worker logs.",
            )),
        },
    }
}

fn worker_diagnostic(observation: &crate::control::WorkerObservation) -> ReadinessDiagnostic {
    let status = &observation.status;
    if observation.stale {
        return ReadinessDiagnostic {
            component: "worker",
            message: format!(
                "worker {} heartbeat is stale; last seen at {}",
                status.worker_id, status.last_heartbeat_at
            ),
            action: "Start or restart the worker and check its logs.".to_string(),
        };
    }

    let message = match status.state {
        crate::control::WorkerState::Starting => {
            format!("worker {} is still starting", status.worker_id)
        }
        crate::control::WorkerState::Stopping => {
            format!("worker {} is stopping", status.worker_id)
        }
        crate::control::WorkerState::Stopped => {
            format!("worker {} is stopped", status.worker_id)
        }
        crate::control::WorkerState::Failed => match &status.last_error {
            Some(error) => format!("worker {} failed: {error}", status.worker_id),
            None => format!("worker {} failed without an error detail", status.worker_id),
        },
        crate::control::WorkerState::Ready | crate::control::WorkerState::Running => {
            format!("worker {} is unavailable", status.worker_id)
        }
    };
    ReadinessDiagnostic {
        component: "worker",
        message,
        action: "Check the worker logs and restart it if needed.".to_string(),
    }
}
