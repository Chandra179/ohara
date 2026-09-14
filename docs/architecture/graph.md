# Graph module

The graph process consumes indexed chunks and writes idempotent `Chunk`,
`Entity`, and `MENTIONS` records to FalkorDB. Its deterministic extractor
produces typed entities for people, organizations, locations, events, concepts,
and products. It accepts explicit typed fields such as `PERSON: Ada Lovelace`
and supplements them with typed lexical cues and organization, location, and
event suffixes. Untyped capitalized phrases are ignored.

Entity resolution normalizes Unicode compatibility forms, removes combining
diacritics, folds case, and normalizes punctuation and whitespace. The entity
type is part of the identity, so a same-named product and organization remain
separate. Explicit aliases resolve to one entity and are retained as aliases.

Graph writes store the display name, type, normalized identity, and aliases.
Deterministic ids and `MERGE` keep replay idempotent, including when an input
artifact is retried.

The version-one labeled resolution fixture covers aliases, acronyms, case and
diacritic variants, same-name different-type values, ambiguous names, and
cross-document mentions. `make entity-resolution-quality` sweeps candidate
merge thresholds and reports precision, recall, F1, false merges, and missed
merges. The selected threshold is `0.98`: candidates below it remain
unresolved. Exact normalized identities and explicit aliases are still the
only automatic production merges; approximate merging is measured but not
silently enabled by the graph writer.

It accepts indexed artifact schema version `1` and rejects incompatible
versions before writing the graph.

The process owns graph persistence and does not alter chunks or vector payloads.
Extraction and resolution stay behind the graph process's internal Seam; no
Rust dependency or indexed artifact change is needed, and retrieval continues
to use the bounded `Chunk` → `Entity` path signal.
