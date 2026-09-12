# Frontend TODO

Implementation queue for the approved design in
[design/DESIGN.md](design/DESIGN.md). Complete each vertical slice with
loading, empty, unavailable, and failure states before moving to the next
priority.

## P0 — live integration correctness

- [x] Fix the live metrics `llmUsage` casing mismatch so Operations cannot crash
  on undefined usage fields.
- [x] Preserve the distinction between unavailable LLM synthesis and an
  ungrounded answer in the HTTP response mapping.
- [x] Make every HTTP-backed route render an explicit unavailable/error state;
  no failed API response may produce a blank page.
- [x] Add live Rust-backed Playwright coverage for health, metrics, and query,
  including unavailable knowledge/model fixtures.

## P1 — complete local product workflows

- [x] Add paginated document listing with backend status preservation and wire
  the Documents view.
- [x] Add document and queue read models for the Overview view.
- [x] Align frontend document/status models with the backend schema before
  enabling filters; the UI preserves all durable statuses and does not invent a
  document-type field.
- [x] Add entity-review listing and merge-preview endpoints and wire the
  Entities view.
- [x] Extend live Rust-backed Playwright coverage to documents and entity review.
- [x] Add bounded topic discovery and queueing from Overview, including
  normalization, duplicate reporting, and a live browser contract.

## P2 — safe operations

- [x] Map durable worker-process readiness, lifecycle state, and stale heartbeat
  diagnostics in the shared shell.
- [ ] Extend the loopback-only topic mutation with CORS and local
  authentication/CSRF behavior before exposing broader browser-triggered
  mutations.
- [ ] Add lifecycle actions only after confirmation, authorization, idempotency,
  and failure behavior are covered by the API contract.
- [x] Keep the mock adapter injectable for unit tests and local UI demos.

## P3 — quality and observability

- [ ] Add the Quality Lab after retrieval and ER measurement contracts are
  stable.
- [ ] Add stage throughput and latency charts only when backend metrics are
  available; never fabricate operational data in production UI.
- [x] Show actionable model and knowledge-store readiness diagnostics in the
  shell and affected pages.

## P4 — release checks

- [x] Run the production frontend build in CI.
- [ ] Audit the production bundle to verify that no secrets or runtime data are
  bundled.
- [ ] Run format, lint, unit, integration, accessibility, and end-to-end checks
  before the first frontend release.
- [ ] Keep `design/DESIGN.md` current when the visual system changes.

## Completed maintenance

- [x] Remove the unsupported entity-merge mutation from the read-only client
  interface and keep merge preview read-only until lifecycle contracts exist.
- [x] Align mock metrics fixtures with durable backend document statuses.
- [x] Keep frontend documentation and CI expectations aligned with the live
  read-only workflow.
- [x] Run frontend lint, unit tests, production build, and mock browser tests in
  repository CI.

## Completed foundation

- [x] Create the React + TypeScript + Vite application in this directory.
- [x] Add Tailwind CSS and design tokens from `design/DESIGN.md`.
- [x] Implement reusable UI primitives, loading/empty/error states, and
  accessible notifications.
- [x] Implement the shared shell, responsive layout, focus styles, and route
  boundaries.
- [x] Define typed frontend models and a mock-adapter API boundary.
- [x] Add Vitest, Testing Library, and Playwright smoke-test configuration.
- [x] Implement the Overview, Documents, Query, Entities, and Operations
  screens against the mock adapter.
- [x] Verify keyboard navigation, focus management, contrast, reduced motion,
  responsive layouts, retry, and refresh behavior.
- [x] Add the HTTP adapter, Vite `/api` proxy, health/status mapping, metrics
  wiring, and query wiring.
- [x] Document frontend local development, environment variables, and API
  setup in the frontend README.
