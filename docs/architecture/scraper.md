# Scraper module

The scraper owns network egress for topic discovery and document fetching. Its
external Interface is `POST /scrape` with a topic and a bounded article limit.

It normalizes HTTP(S) URLs, removes common tracking parameters, fetches accepted
pages, and writes raw HTML plus a JSON handoff to the cleaning inbox. The raw
handoff is schema version `1`. URL hashes are document identities; an existing
catalog entry is reported as a duplicate.

## Provider configuration

The scraper loads `scraper/config.yaml` once during startup. The file owns the
scraper bind address, discovery provider, provider endpoint and locale, and
page-fetch adapter. `OHARA_SCRAPER_CONFIG` may point to another YAML file for
an isolated test or deployment. The committed file is the default source of
configuration; its `${ENV_VAR:-default}` values are only deployment overrides.

The `search.provider` field selects the discovery adapter:

- `bing-news` (default) — Bing News RSS.
- `google-news` — Google News RSS with the configured locale.
- `brave` — Brave Search JSON API; requires
  `OHARA_SCRAPER_BRAVE_API_KEY`.
- `duckduckgo` — DuckDuckGo's HTML search page rendered by the configured
  Obscura binary. DuckDuckGo does not expose an official full web-results API.
- `rss` — a caller-provided RSS or Atom endpoint, useful for local fixtures or
  an internal search gateway.

`search.url` overrides the selected provider endpoint. It is required for
`rss`. `search.country` and `search.language` configure Brave and Google News
locale parameters. `search.brave_api_key` is normally populated from
`OHARA_SCRAPER_BRAVE_API_KEY`; secrets are never sent to the frontend.

`fetch.kind` is `http` (default) or `obscura`. The latter runs Obscura's
documented `fetch --dump html` command for JavaScript-rendered pages;
`fetch.obscura_binary` selects the binary path. Obscura is optional and is not
bundled into the default lightweight scraper image.

When `OHARA_PROCESS_AUTH_TOKEN` is non-empty, `POST /scrape` requires an exact
`Authorization: Bearer <token>` header and returns `401` otherwise. `/health`
remains unauthenticated so readiness checks do not need the secret.

The scraper does not clean HTML, call Qdrant, call FalkorDB, or answer user
queries. Its search and page-fetch adapters are replaceable without changing
the cleaning Interface.
