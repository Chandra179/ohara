//! Deterministic representative corpus and artifact payloads.

use crate::Result;
use crate::artifacts::{atomic_write, seed_directories, write_json};
use serde_json::{Value, json};
use std::path::Path;

/// A representative document shared by process resource workloads.
#[derive(Clone, Debug)]
pub(crate) struct CorpusDocument {
    /// Deterministic document identity.
    pub(crate) document_id: String,
    /// Human-readable title.
    pub(crate) title: String,
    /// Canonical source URL.
    pub(crate) source_url: String,
    /// Raw HTML representation.
    pub(crate) html: String,
    /// Clean Markdown representation.
    pub(crate) markdown: String,
}

/// Build the deterministic representative corpus.
pub(crate) fn build_corpus(count: usize) -> Vec<CorpusDocument> {
    let paragraph = "Ohara is a private local knowledge base for turning web topics into searchable evidence. The scraper discovers source pages, cleaning extracts readable article text, and the indexer creates bounded chunks with deterministic identities. Retrieval ranks those chunks and returns a grounded answer with citations. This representative paragraph contains enough natural language to exercise extraction, normalization, chunking, vector publication, graph mentions, and query synthesis.";
    (0..count)
        .map(|index| {
            let document_id = format!("resource-document-{index:02}");
            let title = format!("Ohara Resource Benchmark Article {index:02}");
            let paragraphs = (0..12)
                .map(|part| format!("{paragraph} Corpus record {index:02}, section {part}."))
                .collect::<Vec<_>>();
            let markdown = format!(
                "# {title}\n\nPRODUCT: Ohara.\n\n{}",
                paragraphs.join("\n\n")
            );
            let body = paragraphs.iter().fold(String::from("<p>PRODUCT: Ohara.</p>"), |mut output, item| {
                use std::fmt::Write as _;
                let _ = write!(output, "<p>{}</p>", html_escape(item));
                output
            });
            let html = format!(
                "<!doctype html><html><head><title>{}</title></head><body><article><h1>{}</h1>{body}</article></body></html>",
                html_escape(&title),
                html_escape(&title),
            );
            CorpusDocument {
                document_id,
                title,
                source_url: format!("https://example.com/resource/{index}"),
                html,
                markdown,
            }
        })
        .collect()
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Seed raw artifacts and cleaning inbox items.
pub(crate) fn seed_cleaning(data_dir: &Path, documents: &[CorpusDocument]) -> Result<()> {
    seed_directories(data_dir)?;
    for document in documents {
        atomic_write(
            &data_dir
                .join("raw")
                .join(format!("{}.html", document.document_id)),
            document.html.as_bytes(),
        )?;
        write_json(
            &data_dir
                .join("catalog")
                .join(format!("{}.json", document.document_id)),
            &catalog_value(document),
        )?;
        write_json(
            &data_dir
                .join("inbox/cleaning")
                .join(format!("{}.json", document.document_id)),
            &raw_artifact(document),
        )?;
    }
    Ok(())
}

/// Seed clean artifacts and indexer inbox items.
pub(crate) fn seed_indexer(data_dir: &Path, documents: &[CorpusDocument]) -> Result<()> {
    seed_directories(data_dir)?;
    for document in documents {
        write_json(
            &data_dir
                .join("catalog")
                .join(format!("{}.json", document.document_id)),
            &catalog_value(document),
        )?;
        write_json(
            &data_dir
                .join("inbox/indexer")
                .join(format!("{}.json", document.document_id)),
            &clean_artifact(document),
        )?;
    }
    Ok(())
}

/// Seed indexed artifacts and graph inbox items.
pub(crate) fn seed_graph(data_dir: &Path, documents: &[CorpusDocument]) -> Result<()> {
    seed_directories(data_dir)?;
    for document in documents {
        write_json(
            &data_dir
                .join("inbox/graph")
                .join(format!("{}.json", document.document_id)),
            &indexed_artifact(document),
        )?;
    }
    Ok(())
}

fn catalog_value(document: &CorpusDocument) -> Value {
    json!({
        "id": document.document_id,
        "sourceUrl": document.source_url,
        "title": document.title,
        "status": "NEW",
        "chunkCount": 0,
        "createdAt": "resource-benchmark",
        "lastProcessedAt": null,
        "error": null,
    })
}

fn raw_artifact(document: &CorpusDocument) -> Value {
    json!({
        "schema_version": 1,
        "document_id": document.document_id,
        "source_url": document.source_url,
        "title": document.title,
        "raw_path": format!("raw/{}.html", document.document_id),
    })
}

fn clean_artifact(document: &CorpusDocument) -> Value {
    json!({
        "schema_version": 1,
        "document_id": document.document_id,
        "source_url": document.source_url,
        "title": document.title,
        "markdown": document.markdown,
    })
}

fn indexed_artifact(document: &CorpusDocument) -> Value {
    let chunks = document
        .markdown
        .split("\n\n")
        .enumerate()
        .map(|(sequence, text)| {
            json!({
                "chunk_id": format!("{}-chunk-{sequence}", document.document_id),
                "document_id": document.document_id,
                "title": document.title,
                "source_url": document.source_url,
                "text": text,
                "sequence": sequence,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": 1,
        "document_id": document.document_id,
        "source_url": document.source_url,
        "title": document.title,
        "chunks": chunks,
        "indexed_at": "resource-benchmark",
    })
}
