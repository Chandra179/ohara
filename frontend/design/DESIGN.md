# Ohara frontend design

This directory contains the approved visual reference for the first frontend
iteration:

[Open the UI prototype](ohara-ui-prototype.png)

The design is intentionally simple and hierarchy-first. It supports four
primary workflows: monitor ingestion, browse documents, ask questions, and
review entity matches.

## Visual direction

- Dark, local-first workbench with calm technical styling.
- Deep navy background with flat surfaces and subtle borders.
- Teal is the primary accent; amber and red are reserved for pending and
  failed states.
- Generous whitespace, short labels, and one clear primary action per view.
- Avoid dense dashboard grids, gradients, glass effects, and decorative UI.

### Tokens

| Token | Value | Use |
| --- | --- | --- |
| `--color-bg` | `#0B1020` | Application background |
| `--color-surface` | `#121A2A` | Panels and cards |
| `--color-border` | `#243149` | Dividers and control boundaries |
| `--color-text` | `#F3F4F6` | Primary text |
| `--color-muted` | `#94A3B8` | Supporting text |
| `--color-accent` | `#55D6C2` | Links, focus, and primary actions |
| `--color-pending` | `#F5C451` | Processing and pending states |
| `--color-danger` | `#FF6B5F` | Failed states |

## Screen responsibilities

### Overview

Show local health, indexed/processing/failed totals, and the latest ingestion
queue. Keep the queue actionable and link to the full Documents view.

### Query

Provide one prominent search field, a readable answer area, and compact
citations. Loading, empty, unavailable-model, and ungrounded-answer states
must be designed before the real query integration.

### Documents

Provide searchable, filterable document browsing with status, source, type,
and last-updated information. A document detail view can follow the first
vertical slice.

### Entities

Show pending review candidates and a clear merge preview. The merge action must
state which entity survives and remain reversible until the user confirms it.

Quality Lab and Operations are secondary navigation destinations. They should
not compete with the four primary workflows in the first release.

## Interaction rules

- Use visible keyboard focus and preserve a logical tab order.
- Every async operation has loading, success, empty, and failure states.
- Destructive or identity-changing actions require an explicit confirmation.
- Status is communicated with text and color, never color alone.
- Keep the primary navigation stable across screens.

## Implementation boundary

The frontend should consume a typed HTTP/JSON boundary when one is available.
It must not access SQLite, LadybugDB, Rust modules, or local runtime files
directly. Until the API contract exists, use typed fixtures or a mock adapter
behind the same client interface.
