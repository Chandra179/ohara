# Runtime composition and readiness

Runtime composition is the only place that selects concrete providers. It
connects the fetch engine, SQLite control store, Qdrant/FalkorDB knowledge
adapter, local embedder, and language-model adapter to the pipeline ports.

The worker opens a writable knowledge adapter and publishes lifecycle and
heartbeat state in SQLite. The API lazily caches expensive model providers but
creates a knowledge client for each query/readiness operation, so service
changes and worker writes are visible without restarting the API.

Readiness checks SQLite, Qdrant, FalkorDB, the embedding model cache, Ollama,
and the worker heartbeat concurrently. Missing dependencies produce actionable
diagnostics; they do not trigger hidden downloads or repair operations.

The worker and operator mutations use the runtime lock. Query traffic is
read-only and does not supervise the worker. Explicit ports remain available
for hermetic tests and deployments that provide their own services.
