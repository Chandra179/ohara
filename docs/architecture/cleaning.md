# Cleaning module

The cleaning process consumes raw-artifact metadata from its inbox. It uses a
readability extractor followed by Markdown conversion, removes unsafe embedded
data and excess whitespace, detects the language, and rejects content that is
too short.

Accepted output is a clean Markdown artifact and one indexing-inbox message.
The module owns content quality decisions; it does not create vectors or write
graph data. It accepts raw artifact schema version `1` and publishes clean
artifact schema version `1`. A failed item is recorded in the catalog and the
process continues with the next inbox item.
