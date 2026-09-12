# Runtime composition and readiness

Runtime services assemble the default local providers and expose them to the
worker and operator query flow through behavioral ports. This is the
composition root: it selects concrete adapters, validates model and storage
compatibility, and acquires the process lock for mutating work.

The API keeps a process-local query runtime. Query Adapters are assembled
lazily on the first request that can pass readiness checks, then reused through
cloned behavioral ports. Failed construction is retryable, so repairing a
missing model or knowledge artifact does not require restarting the API.

The pipeline does not construct network clients, database handles, or vendor
stores. The Engine owns outbound HTTP adapters, Control owns SQLite, and
Knowledge owns the graph and vector implementation. Runtime composition is the
small place where those planes are connected.

## Readiness

Readiness checks probe the configured control store, knowledge store, embedding
model cache, and language-model endpoint concurrently. The result is a
provider-neutral report containing availability plus an actionable diagnostic.
The local API maps that report to its health response; handlers do not know how
any provider is opened or checked.

An unavailable language model degrades synthesis while retrieval remains useful.
An unavailable control store, knowledge store, or embedder makes the service
offline because those components are required for the current local query
path. Missing model files and invalid knowledge artifacts are reported with a
recovery action instead of an opaque server failure.

## Provider substitution

The worker accepts explicit ports for tests and remote-provider deployments.
Default assembly remains feature-gated for the native LadybugDB and ONNX
stacks. A build without those features must inject compatible ports rather than
silently falling back to a different datastore or model.
