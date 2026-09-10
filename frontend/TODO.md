# Frontend TODO

Implementation order for the approved design in
[design/DESIGN.md](design/DESIGN.md). Complete each vertical slice with its
loading, empty, and failure states before moving to the next priority.

## P0 — foundation

- [x] Create the React + TypeScript + Vite application in this directory.
- [x] Add Tailwind CSS and define the design tokens from `design/DESIGN.md`.
- [x] Define and implement the reusable UI foundation: buttons, inputs,
  selects, panels, badges, tables, modal, and loading/empty/error states.
- [x] Add the shared application shell: sidebar, top bar, responsive layout,
  focus styles, and route boundaries.
- [x] Add typed frontend models and an API client/mock-adapter boundary. Keep
  backend and datastore details out of UI components.
- [x] Add Vitest, Testing Library, and Playwright smoke-test configuration.

## P1 — first usable workflow

- [ ] Implement the Overview screen with health, document totals, and the
  ingestion queue.
- [ ] Implement the Documents screen with search, status/type/source filters,
  pagination, and document status states.
- [ ] Implement the Query screen with search, answer, citations, loading,
  empty, unavailable-LLM, and ungrounded-answer states.
- [ ] Implement the Entities screen with pending review, merge preview,
  confirmation, success, and failure states.
- [ ] Add route-level and component-level tests for the four primary screens.

## P2 — usability and resilience

- [ ] Verify keyboard navigation, focus management, contrast, and reduced
  motion behavior.
- [ ] Add responsive layouts for tablet and narrow desktop widths.
- [ ] Add retry and refresh behavior for asynchronous operations.
- [ ] Add accessible notifications for ingestion, query, and merge outcomes.
- [ ] Add Playwright coverage for the query and entity-merge journeys.

## P3 — secondary operations

- [ ] Define the API contract for metrics before implementing the Operations
  screen.
- [ ] Add the Operations view for queue health, stage outcomes, and usage
  summaries.
- [ ] Add the Quality Lab view after retrieval and ER measurement contracts
  are stable.
- [ ] Add stage throughput and latency charts only when backend metrics are
  available; do not fabricate operational data in production UI.

## Documentation and release checks

- [ ] Keep `design/DESIGN.md` current when the visual system changes.
- [ ] Document local development, environment variables, and API setup in a
  frontend README.
- [ ] Add a production build check and verify that no secrets or runtime data
  are bundled.
- [ ] Run format, lint, unit, integration, accessibility, and end-to-end
  checks before the first frontend release.
