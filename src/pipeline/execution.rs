//! Job execution and stage-specific contexts.
//!
//! The worker scheduler claims jobs, then hands one claim to this module. This
//! module owns dispatch, panic isolation, retry classification, stage chaining,
//! and audit writes. Each stage receives only the ports it can use, keeping the
//! stage interface small and its tests honest.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::config::Config;
use crate::control::{self, ClaimedJob, Completion, ControlDb, DbError, Stage};
use crate::engine::Fetcher;
use crate::knowledge::KnowledgeStore;
use crate::llm::Llm;

use super::clean::Extractor;
use super::embed::Embedder;
use super::{StageError, StageOutcome, clean, embed, extract, scrape};
use crate::runtime::WorkerPorts;

/// What the loop does after one claimed job's outcome is recorded.
pub(crate) enum Flow {
    /// Keep scheduling.
    Continue,
    /// A `Fatal` classification (§6): stop scheduling, drain, reconcile at next boot.
    Abort(Box<StageError>),
}

/// The inputs shared by the scheduler and one stage execution.
pub(crate) struct StageExecutor<'a> {
    config: &'a Config,
    conn: &'a ControlDb,
    handle: &'a tokio::runtime::Handle,
    ports: &'a WorkerPorts,
}

impl<'a> StageExecutor<'a> {
    /// Creates an executor over the ports owned by a worker.
    pub(crate) fn new(
        config: &'a Config,
        conn: &'a ControlDb,
        handle: &'a tokio::runtime::Handle,
        ports: &'a WorkerPorts,
    ) -> Self {
        Self {
            config,
            conn,
            handle,
            ports,
        }
    }

    /// Executes one claimed job and records its state-machine outcome.
    pub(crate) fn execute(&self, stage: Stage, job: &ClaimedJob) -> Result<Flow, DbError> {
        let now = control::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| self.dispatch(stage, job)));
        match outcome {
            Ok(Ok(outcome)) => {
                let completion = match (outcome, stage) {
                    (StageOutcome::Stop, _) => Completion::Done,
                    (StageOutcome::Advance, Stage::Vectorize)
                        if !self.config.pipeline().graph_enabled() =>
                    {
                        Completion::Milestone
                    }
                    (StageOutcome::Advance, _) => Completion::Chain,
                };
                control::complete_with_event(self.conn, stage, job, completion, &now)?;
                Ok(Flow::Continue)
            }
            Ok(Err(err)) => match &err {
                StageError::Transient { .. } => {
                    let attempts_after = job.attempts() + 1;
                    if attempts_after >= job.max_attempts() {
                        control::dead_with_event(
                            self.conn,
                            job.job_id(),
                            job.doc_id(),
                            stage,
                            &err.to_string(),
                            &now,
                        )?;
                    } else {
                        let due = control::now_plus(&now, self.backoff_secs(attempts_after))?;
                        control::retry_with_event(
                            self.conn,
                            job.job_id(),
                            job.doc_id(),
                            stage,
                            &due,
                            &err.to_string(),
                            &now,
                        )?;
                    }
                    Ok(Flow::Continue)
                }
                StageError::Permanent { .. } => {
                    control::dead_with_event(
                        self.conn,
                        job.job_id(),
                        job.doc_id(),
                        stage,
                        &err.to_string(),
                        &now,
                    )?;
                    Ok(Flow::Continue)
                }
                StageError::Fatal { .. } => {
                    control::record_event(
                        self.conn,
                        Some(job.doc_id()),
                        Some(job.job_id()),
                        Some(stage.as_str()),
                        "FATAL",
                        Some(&err.to_string()),
                    )?;
                    Ok(Flow::Abort(Box::new(err)))
                }
            },
            Err(panic) => {
                let detail = panic_detail(&panic);
                control::record_event(
                    self.conn,
                    Some(job.doc_id()),
                    Some(job.job_id()),
                    Some(stage.as_str()),
                    "PANIC",
                    Some(&detail),
                )?;
                Ok(Flow::Continue)
            }
        }
    }

    fn dispatch(&self, stage: Stage, job: &ClaimedJob) -> Result<StageOutcome, StageError> {
        match stage {
            Stage::Scrape => scrape::run(&self.scrape_context(), job),
            Stage::Clean => clean::run(&self.clean_context(), job),
            Stage::Vectorize => embed::run(&self.embed_context(), job),
            Stage::Extract => extract::run(&self.extract_context(), job),
        }
    }

    fn scrape_context(&self) -> ScrapeContext<'_> {
        ScrapeContext {
            config: self.config,
            conn: self.conn,
            handle: self.handle,
            fetcher: self.ports.fetcher.as_ref(),
        }
    }

    fn clean_context(&self) -> CleanContext<'_> {
        CleanContext {
            config: self.config,
            conn: self.conn,
            extractor: self.ports.extractor.as_ref(),
        }
    }

    fn embed_context(&self) -> EmbedContext<'_> {
        EmbedContext {
            config: self.config,
            conn: self.conn,
            handle: self.handle,
            embedder: self.ports.embedder.as_ref(),
            knowledge: self.ports.knowledge.as_ref(),
        }
    }

    fn extract_context(&self) -> ExtractContext<'_> {
        ExtractContext {
            config: self.config,
            conn: self.conn,
            handle: self.handle,
            embedder: self.ports.embedder.as_ref(),
            knowledge: self.ports.knowledge.as_ref(),
            llm: self.ports.llm.as_ref(),
        }
    }

    fn backoff_secs(&self, attempts_after: i64) -> u64 {
        let base = self.config.backoff_base().as_secs();
        let exp = u32::try_from(attempts_after.saturating_sub(1))
            .unwrap_or(crate::config::defaults::RETRY_EXPONENT_CAP)
            .min(crate::config::defaults::RETRY_EXPONENT_CAP);
        let secs = base.saturating_mul(2_u64.saturating_pow(exp));
        let jitter_salt = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| u64::from(time.subsec_nanos()));
        let jitter_window =
            secs.saturating_mul(crate::config::defaults::RETRY_JITTER_PERCENT) / 100 + 1;
        secs.saturating_add(jitter_salt % jitter_window)
    }
}

/// Dependencies available to Stage 1.
pub(crate) struct ScrapeContext<'a> {
    pub(crate) config: &'a Config,
    pub(crate) conn: &'a ControlDb,
    pub(crate) handle: &'a tokio::runtime::Handle,
    pub(crate) fetcher: &'a dyn Fetcher,
}

/// Dependencies available to Stage 2.
pub(crate) struct CleanContext<'a> {
    pub(crate) config: &'a Config,
    pub(crate) conn: &'a ControlDb,
    pub(crate) extractor: &'a dyn Extractor,
}

/// Dependencies available to Stage 3.
pub(crate) struct EmbedContext<'a> {
    pub(crate) config: &'a Config,
    pub(crate) conn: &'a ControlDb,
    pub(crate) handle: &'a tokio::runtime::Handle,
    pub(crate) embedder: &'a dyn Embedder,
    pub(crate) knowledge: &'a dyn KnowledgeStore,
}

/// Dependencies available to Stage 4.
pub(crate) struct ExtractContext<'a> {
    pub(crate) config: &'a Config,
    pub(crate) conn: &'a ControlDb,
    pub(crate) handle: &'a tokio::runtime::Handle,
    pub(crate) embedder: &'a dyn Embedder,
    pub(crate) knowledge: &'a dyn KnowledgeStore,
    pub(crate) llm: &'a dyn Llm,
}

/// Best-effort panic payload extraction (§10).
fn panic_detail(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}
