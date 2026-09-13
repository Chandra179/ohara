# Scraper module

The scraper owns network egress for topic discovery and document fetching. Its
external Interface is `POST /scrape` with a topic and a bounded article limit.

It searches Bing News RSS, normalizes HTTP(S) URLs, removes common tracking
parameters, fetches accepted pages, and writes raw HTML plus a JSON handoff to
the cleaning inbox. The raw handoff is schema version `1`. URL hashes are
document identities; an existing catalog entry is reported as a duplicate.

The scraper does not clean HTML, call Qdrant, call FalkorDB, or answer user
queries. Its Adapter is replaceable without changing the cleaning Interface.
