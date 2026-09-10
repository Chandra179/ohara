# Ohara Domain Context

Ohara ingests documents into a local GraphRAG index and records the work and
provider activity needed to reproduce, audit, and operate that index.

## Language

**Document**:
A registered source whose fetched content moves through the scrape, clean,
vectorize, and optional graph-extraction stages.

**Job**:
The durable execution record for one stage of one Document.

**Completion Attempt**:
One request made to an LLM provider for a stage or operator query, regardless
of whether the provider succeeds.

**Usage Ledger**:
An immutable control-plane record for each Completion Attempt, including its
provider, model, token counts, outcome, and cost estimate.

**Quality Fallback**:
A second bounded synthesis attempt through a configured model after the primary
attempt fails at the provider boundary or produces invalid grounding.

**GC Candidate**:
An unmerged entity with a durable `zero_since` observation proving that no graph
mentions were present; it becomes collectible only after the configured grace
period and a final relationship recheck.

**Fetcher Contract**:
The shared behavioral guarantees every fetch Adapter must satisfy, including
policy enforcement, conditional 304 handling, response-size limits, and error
classification.

## Relationships

- A **Document** produces one **Job** per pipeline stage.
- A **Completion Attempt** may be associated with a **Document** and **Job**,
  or with an operator query.
- A **Usage Ledger** records exactly one **Completion Attempt**.
- A **Quality Fallback** is a second Completion Attempt and therefore gets its
  own Usage Ledger record.
- A **GC Candidate** is discovered by an operator sweep and is deleted only when
  the knowledge-facing graph recheck still finds no relationships.

## Example dialogue

> **Dev:** "The extraction request failed; should the usage count increase?"
> **Domain expert:** "Yes. It is still a **Completion Attempt**, so the
> **Usage Ledger** must retain its egress and token usage when available."

## Flagged ambiguities

- "Usage counter" previously meant the in-memory cumulative value. Resolved:
  durable per-attempt records are the **Usage Ledger**; counters and costs are
  derived metrics.
- "Entity deletion" previously had no explicit boundary. Resolved: the
  `KnowledgeStore::delete_entity` Interface owns graph/vector deletion, while
  the `ops::collect_entity_garbage` Adapter coordinates it with the control
  registry under the runtime lock.
