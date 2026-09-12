# Language-model services

The language-model boundary owns provider identity, completion responses,
structured-output support, and provider error mapping. It does not own pipeline
state, prompt-side graph effects, or the networking mechanism used by an
adapter. Network adapters live with the Engine plane; callers depend on this
provider-neutral port.

## Current behavior

The default provider is local Ollama. Extraction and synthesis use bounded
requests; synthesis accepts only grounded answers with exact evidence citations.
A configured different fallback model gets one additional bounded attempt after
provider or grounding failure. Ranked retrieval remains the final fallback.

Every completion attempt, including failures, is recorded with provider, model,
outcome, token counts, and configured cost estimates. This durable Usage Ledger
is aggregated by the control plane after restart.

Cloud providers are not implemented. Enabling cloud egress must remain explicit,
audited, and covered by the LLM port contract.
