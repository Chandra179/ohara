# Network engine

The engine owns all outbound network activity and implements the fetcher port.
It also contains HTTP adapters for provider ports such as local Ollama and
topic discovery over Bing News RSS. It is responsible for safe, policy-
compliant egress rather than document state, language-model semantics, or
pipeline scheduling.

## Provider ladder

The default ladder tries plain HTTP first, then a browser-profile request
adapter. An optional executable adapter can provide JavaScript rendering and
stealth. The ladder starts at the requested capability and escalates only for
anti-bot or JavaScript-required outcomes. Permanent failures stop the ladder.

Every provider reports capabilities honestly. Browser-profile requests do not
claim JavaScript execution, and the optional executable must use the versioned
JSON-line protocol.

## Shared policy

The engine validates HTTP(S) URLs, blocks private and loopback destinations by
default, re-validates every redirect hop, enforces a response-body limit, and
honors robots and per-host politeness limits. Conditional validators are passed
for recrawls. A 304 is usable only when the local payload still exists.

URL normalization is performed once at the boundary for stable URL deduplication.
Provider-native errors are mapped into the common fetch error taxonomy so the
pipeline can make retry decisions without knowing the provider.

Topic discovery returns validated, normalized HTTP(S) destinations and never
follows a provider redirect just to extract a result. Its bounded RSS response
is parsed inside the engine; the API receives only provider-neutral titles and
URLs.
