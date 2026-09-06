//! Boot-time reconciliation posture (§7.3): `SQLite` intent vs. what actually
//! landed in the knowledge plane.
//!
//! There is deliberately no separate sweep pass for vector or graph state: every
//! knowledge-plane write is replay-idempotent (§7.1), and the worker's lease
//! reclaim hands an interrupted job back to the queue with its attempts budget
//! intact. A `VECTORIZE` job that died mid-write re-runs into the registry
//! comparison in [`crate::pipeline::embed`] — chunks whose rows match are kept
//! and their missing vectors repaired; changed content deletes first (§7.4).
//! Entity/fact writes (Stage 4) re-merge on replay by the same principle, so a
//! dedicated boot sweep would duplicate that machinery without adding recovery
//! power. What *is* swept at boot: pending deletion intents and expired
//! `stage_events` — see [`crate::control::reconcile`].
