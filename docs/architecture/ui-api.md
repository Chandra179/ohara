# Frontend and local API

The frontend is a separate client with one typed API boundary. It owns layout,
view state, loading/empty/error states, notifications, and user interaction.
It does not know about SQLite, LadybugDB, provider SDKs, filesystem paths, or
CLI subprocesses.

## Current shape

The mock adapter remains the default for offline design and unit tests. The HTTP
adapter is selected for the local development launcher and currently supports:

- health and service-status display;
- operator metrics for the Operations view; and
- query submission with grounded, ungrounded, unavailable, and ranked-source
  fallback states.

The Overview, Documents, and Entities screens are implemented visually but use
unsupported HTTP methods until their read models and contracts land. Lifecycle
actions are intentionally not exposed yet. Frontend document type/status models
are provisional: the current control schema has no document-type field and has
more statuses than the UI model.

## Readiness for live use

The live frontend is not release-ready until the nested metrics usage DTO is
camel-cased, all unsupported screens have explicit unavailable states, and a
Rust-backed browser suite covers health, metrics, query, and entity-review
flows. The HTTP adapter should preserve the distinction between an unavailable
LLM and an ungrounded answer. Missing models or invalid local knowledge
artifacts must surface as actionable readiness errors rather than blank pages.

The current metrics response has a nested LLM-usage casing mismatch with the
frontend contract. Health also probes concrete local providers at the transport
boundary; a shared readiness seam should be introduced before the API grows
more provider-specific checks.
