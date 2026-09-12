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

## P1 — complete read-only product workflows

- [x] Add paginated document listing with backend status preservation and wire
  the Documents view.
- [x] Add document and queue read models for the Overview view.
- [x] Align frontend document/status models with the backend schema before
  enabling filters; the UI preserves all durable statuses and does not invent a
  document-type field.
- [x] Add entity-review listing and merge-preview endpoints and wire the
  Entities view.
- [x] Extend live Rust-backed Playwright coverage to documents and entity review.

## P2 — safe operations

- [ ] Map worker-process readiness after the Rust server can supervise or
  observe ingestion.
- [ ] Define request limits, CORS, and local authentication/CSRF behavior before
  exposing browser-triggered mutations.
- [ ] Add lifecycle actions only after confirmation, authorization, idempotency,
  and failure behavior are covered by the API contract.
- [ ] Keep the mock adapter injectable for unit tests and local UI demos while
  replacing unsupported HTTP methods incrementally.

## P3 — quality and observability

- [ ] Add the Quality Lab after retrieval and ER measurement contracts are
  stable.
- [ ] Add stage throughput and latency charts only when backend metrics are
  available; never fabricate operational data in production UI.
- [ ] Show actionable model and knowledge-store readiness diagnostics in the
  shell and affected pages.

## P4 — release checks

- [ ] Add a production build check and verify that no secrets or runtime data
  are bundled.
- [ ] Run format, lint, unit, integration, accessibility, and end-to-end checks
  before the first frontend release.
- [ ] Keep `design/DESIGN.md` current when the visual system changes.

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
