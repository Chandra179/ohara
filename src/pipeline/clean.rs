//! Stage 2 — Clean (§8): boilerplate removal → Markdown → sanitize → hash/dedup →
//! quality + language gate, and the [`Extractor`] port (§1.3) with its
//! `readability` + `html2md` implementation.

use unicode_normalization::UnicodeNormalization;

use crate::Class;
use crate::control::{self, ClaimedJob, ControlDb};

use super::execution::CleanContext;
use super::{StageError, StageOutcome};

/// Extracted article (§8 Stage 2): boilerplate removed, structure preserved.
#[derive(Debug, Clone)]
pub struct ExtractedArticle {
    /// Page title, if detected.
    pub title: Option<String>,
    /// Byline, if detected.
    pub byline: Option<String>,
    /// Primary content as Markdown (headings, lists, tables preserved).
    pub markdown: String,
}

/// Extraction failures (§9): `Err` is reserved for "the operation couldn't do its
/// job" — quality outcomes are values in the stage signature (§10).
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// No primary content could be located in the document.
    #[error("no extractable content: {0}")]
    NoContent(String),
    /// The extractor itself failed.
    #[error("extractor failed: {0}")]
    Failed(String),
}

impl ExtractError {
    /// Retry class (§10): a failed extractor may succeed on retry; missing content
    /// is a property of the document.
    #[must_use]
    pub fn class(&self) -> Class {
        match self {
            ExtractError::NoContent(_) => Class::Permanent,
            ExtractError::Failed(_) => Class::Retry,
        }
    }
}

/// The extraction port (§9): HTML → (title, byline, markdown). Pure; relative URLs
/// absolutized; scripts stripped before conversion (§12).
pub trait Extractor: Send + Sync {
    /// Extracts the primary content of `html`, resolving relative URLs against
    /// `base_url`.
    ///
    /// # Errors
    /// [`ExtractError`] — never quality outcomes (§10).
    fn extract(&self, html: &str, base_url: &str) -> Result<ExtractedArticle, ExtractError>;
}

/// The §8 Stage 2 domain outcomes — values, not errors (§10): a rejected or
/// duplicate document is a *result* of the stage, and the job still completes
/// (`DONE`) with nothing chained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanOutcome {
    /// The document passed every gate and was stored.
    Accepted,
    /// A §8 quality gate rejected the document; `FAILED_QUALITY` with the reason.
    Rejected {
        /// Why the document was rejected.
        reason: String,
    },
    /// The cleaned content already exists (`clean_content_hash`, §5) — the job
    /// completes, the document keeps its milestone, nothing chains.
    SkippedDuplicate {
        /// The document that already carries this content.
        of_doc_id: String,
    },
}

/// Paywall indicators (§8 Stage 2: "paywall markers"), matched case-insensitively.
const PAYWALL_MARKERS: &[&str] = &[
    "subscribe to continue reading",
    "subscribe to keep reading",
    "sign in to continue reading",
    "create a free account to continue reading",
    "this article is for subscribers only",
    "you've reached your monthly limit",
    "start your free trial to continue",
];

/// The [`Extractor`] implementation (§2): `readability` for primary-content
/// extraction, `html2md` for the Markdown conversion. Pure — file and store I/O
/// belong to the stage body.
pub struct ReadabilityExtractor;

impl Default for ReadabilityExtractor {
    fn default() -> Self {
        Self
    }
}

impl ReadabilityExtractor {
    /// Builds the extractor.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for ReadabilityExtractor {
    fn extract(&self, html: &str, base_url: &str) -> Result<ExtractedArticle, ExtractError> {
        let url = url::Url::parse(base_url)
            .map_err(|_| ExtractError::NoContent(format!("invalid base url {base_url:?}")))?;
        let mut cursor = std::io::Cursor::new(html);
        let product = readability::extractor::extract(&mut cursor, &url).map_err(|e| match e {
            readability::error::Error::Unexpected => {
                ExtractError::NoContent("no primary content located".to_string())
            }
            other => ExtractError::Failed(other.to_string()),
        })?;
        if product.text.trim().is_empty() {
            return Err(ExtractError::NoContent(
                "extractor produced no text".to_string(),
            ));
        }
        Ok(ExtractedArticle {
            title: if product.title.is_empty() {
                None
            } else {
                Some(product.title)
            },
            byline: None, // readability 0.3 does not extract bylines
            markdown: html2md::parse_html(&product.content),
        })
    }
}

/// Runs the stage for one claimed job: extract → sanitize → gates (§8 Stage 2)
/// → store. Returns [`StageOutcome::Advance`] only for
/// [`CleanOutcome::Accepted`]; rejections and duplicates stop the chain here.
///
/// # Errors
/// [`StageError::Transient`] for local I/O and extractor failures;
/// [`StageError::Fatal`] for store failures (§10).
pub(super) fn run(ctx: &CleanContext<'_>, job: &ClaimedJob) -> Result<StageOutcome, StageError> {
    let doc = control::get(ctx.conn, job.doc_id())?
        .ok_or_else(|| StageError::fatal("claimed job's document row is missing"))?;
    let now = control::now();
    let attempt = u32::try_from(job.attempts() + 1).unwrap_or(u32::MAX);
    let io_err = |e: std::io::Error| StageError::transient(e, Class::Retry, attempt);

    let html = read_gz(std::path::Path::new(&doc.raw_file_path)).map_err(io_err)?;

    let article = match ctx.extractor.extract(&html, &doc.source_url) {
        Ok(article) => article,
        Err(ExtractError::NoContent(reason)) => {
            // Boilerplate-only / no primary content is a §8 quality rejection.
            return reject(ctx.conn, job.doc_id(), &reason, &now)
                .map_err(StageError::from)
                .map(|()| StageOutcome::Stop);
        }
        Err(e @ ExtractError::Failed(_)) => {
            return Err(StageError::transient(e, Class::Retry, attempt));
        }
    };

    let markdown = sanitize(&article.markdown);
    let word_count = i64::try_from(markdown.split_whitespace().count()).unwrap_or(i64::MAX);

    let min_word_count = ctx.config.pipeline().min_word_count();
    if word_count < min_word_count {
        return reject(
            ctx.conn,
            job.doc_id(),
            &format!("word count {word_count} < {min_word_count}"),
            &now,
        )
        .map_err(StageError::from)
        .map(|()| StageOutcome::Stop);
    }
    if let Some(marker) = PAYWALL_MARKERS.iter().find(|m| contains_ci(&markdown, m)) {
        let reason = format!("paywall marker {marker:?}");
        return reject(ctx.conn, job.doc_id(), &reason, &now)
            .map_err(StageError::from)
            .map(|()| StageOutcome::Stop);
    }

    // Language gate (§8 Stage 2): the embedder is English-first — embedding
    // text outside `target_languages` would poison the vector space. Targets
    // are ISO 639-1; whatlang speaks 639-3, so the detected code is mapped.
    let language = whatlang::detect(&markdown).map(|info| iso639_1(info.lang()));
    let accepted = language.as_ref().is_some_and(|code| {
        ctx.config
            .target_languages()
            .iter()
            .any(|t| t.eq_ignore_ascii_case(code))
    });
    if !accepted {
        return reject(
            ctx.conn,
            job.doc_id(),
            &format!(
                "language {language:?} outside target_languages {:?}",
                ctx.config.target_languages()
            ),
            &now,
        )
        .map_err(StageError::from)
        .map(|()| StageOutcome::Stop);
    }

    // Content-level dedup (§8 Stage 2): a document's own hash from a previous
    // cycle is not a duplicate.
    let hash = content_hash(&markdown);
    if let Some(other) = control::find_id_by_content_hash(ctx.conn, &hash)?
        && other != job.doc_id()
    {
        control::record_event(
            ctx.conn,
            Some(job.doc_id()),
            Some(job.job_id()),
            Some(crate::control::Stage::Clean.as_str()),
            "DONE",
            Some(&format!("duplicate of {other}")),
        )?;
        return Ok(StageOutcome::Stop);
    }

    let clean_path = ctx
        .config
        .data_dir()
        .join("clean")
        .join(format!("{}.md", job.doc_id()));
    if let Some(parent) = clean_path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    std::fs::write(&clean_path, &markdown).map_err(io_err)?;
    control::update_clean_result(
        ctx.conn,
        job.doc_id(),
        &control::CleanResult {
            clean_file_path: clean_path.to_string_lossy().into_owned(),
            clean_content_hash: hash,
            title: article.title,
            author: article.byline,
            language,
            word_count,
        },
        &now,
    )?;
    Ok(StageOutcome::Advance)
}

/// Records a §8 quality rejection (`FAILED_QUALITY` + reason) and stops.
fn reject(conn: &ControlDb, doc_id: &str, reason: &str, now: &str) -> Result<(), control::DbError> {
    control::mark_quality_rejected(conn, doc_id, reason, now)
}

/// Reads a gz payload written by Stage 1.
fn read_gz(path: &std::path::Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut decoder = flate2::read::GzDecoder::new(file);
    let mut html = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut html)?;
    Ok(html)
}

/// Sanitization (§8 Stage 2): Unicode NFC, inline `data:`/SVG payloads stripped,
/// trailing whitespace trimmed and blank-line runs collapsed to one blank line
/// (internal spacing is left alone — Markdown structure is data).
#[must_use]
pub fn sanitize(markdown: &str) -> String {
    let nfc: String = markdown.nfc().collect();
    let stripped = strip_data_uris(&nfc);
    let mut out = String::with_capacity(stripped.len());
    let mut blanks = 0usize;
    for line in stripped.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(trimmed);
        out.push('\n');
    }
    out
}

/// Removes Markdown image/link targets with `data:` payloads (inline base64 and
/// SVG, §8): everything from `](data:` through the next `)`.
fn strip_data_uris(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(pos) = rest.find("](data:") {
        // Include the enclosing ![alt / [text opener, not just the target.
        let head = &rest[..pos];
        let start = head.rfind("![").or_else(|| head.rfind('[')).unwrap_or(pos);
        out.push_str(&head[..start]);
        let after = &rest[pos..];
        rest = after.find(')').map_or("", |end| &after[end + 1..]);
    }
    out.push_str(rest);
    out
}

/// Case-insensitive substring check.
fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// ISO 639-1 for the languages with a two-letter code; whatlang's ISO 639-3
/// code otherwise. `target_languages` is user-facing, so it speaks 639-1.
fn iso639_1(lang: whatlang::Lang) -> String {
    use whatlang::Lang;
    let two_letter = match lang {
        Lang::Eng => "en",
        Lang::Fra => "fr",
        Lang::Deu => "de",
        Lang::Spa => "es",
        Lang::Ita => "it",
        Lang::Por => "pt",
        Lang::Rus => "ru",
        Lang::Nld => "nl",
        Lang::Pol => "pl",
        Lang::Tur => "tr",
        Lang::Jpn => "ja",
        Lang::Kor => "ko",
        Lang::Cmn => "zh",
        Lang::Ara => "ar",
        Lang::Hin => "hi",
        Lang::Swe => "sv",
        Lang::Dan => "da",
        Lang::Ces => "cs",
        Lang::Fin => "fi",
        Lang::Ell => "el",
        Lang::Hun => "hu",
        Lang::Ron => "ro",
        Lang::Ukr => "uk",
        Lang::Heb => "he",
        Lang::Tha => "th",
        Lang::Vie => "vi",
        Lang::Ind => "id",
        other => return other.code().to_string(),
    };
    two_letter.to_string()
}

/// `sha256` hex — the §5 `clean_content_hash` (and later `chunk_id`) form.
pub(crate) fn content_hash(data: &str) -> String {
    use std::fmt::Write as _;
    let digest = <sha2::Sha256 as sha2::Digest>::digest(data.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

    use std::sync::Arc;

    use std::path::Path;

    use super::*;
    use crate::config::Config;
    use crate::control::testing::{boot, seed_doc};
    use crate::control::{ClaimedJob, Completion, ControlDb, DocStatus, Stage};

    const NOW: &str = "2026-09-06 12:00:00";

    /// A fixture article long enough to pass the word-count gate.
    const ARTICLE: &str = "SQLite is an embedded database engine. It stores data in a single \
cross-platform file and needs no server process at all. Developers love it \
because the deployment story could hardly be simpler for applications of \
almost every conceivable shape and size today. The library reads and writes \
directly to ordinary disk files, and the complete database with multiple \
tables, indices, triggers, and views lives inside one portable file.";

    /// A config whose `data_dir` is a temp dir, so stage writes stay contained.
    fn fixture_config(dir: &std::path::Path) -> Arc<Config> {
        let toml_path = dir.join("ohara.toml");
        std::fs::write(
            &toml_path,
            format!("data_dir = {:?}\n", dir.join("data").display()),
        )
        .unwrap();
        Arc::new(Config::load(Some(&toml_path)).unwrap())
    }

    /// An extractor port fake (§14): returns a canned article for any input.
    struct FakeExtractor {
        article: ExtractedArticle,
    }

    impl Extractor for FakeExtractor {
        fn extract(&self, _html: &str, _base: &str) -> Result<ExtractedArticle, ExtractError> {
            Ok(self.article.clone())
        }
    }

    /// Writes the raw gz payload the clean stage reads back.
    fn write_raw(config: &Config, doc_id: &str, html: &str) -> String {
        let raw = config
            .data_dir()
            .join("raw")
            .join(format!("{doc_id}.html.gz"));
        std::fs::create_dir_all(raw.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&raw).unwrap();
        let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        std::io::Write::write_all(&mut enc, html.as_bytes()).unwrap();
        enc.finish().unwrap();
        raw.to_string_lossy().into_owned()
    }

    fn ctx_with<'a>(
        config: &'a Arc<Config>,
        conn: &'a ControlDb,
        extractor: &'a dyn Extractor,
    ) -> CleanContext<'a> {
        CleanContext {
            config,
            conn,
            extractor,
        }
    }

    fn claimed(conn: &ControlDb, doc_id: &str, stage: Stage) -> ClaimedJob {
        control::enqueue(conn, "test-job", doc_id, stage, 5, None, NOW).unwrap();
        control::claim_next(conn, stage, "w1", NOW, 60)
            .unwrap()
            .expect("claimable")
    }

    #[tokio::test]
    async fn accepted_document_is_stored_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let config = fixture_config(dir.path());
        let conn = boot();
        let doc_id = seed_doc(&conn, "a");
        let raw_path = write_raw(&config, &doc_id, "<html><body>raw</body></html>");
        conn.raw()
            .execute(
                "UPDATE documents SET raw_file_path = ?1 WHERE doc_id = ?2",
                rusqlite::params![raw_path, doc_id],
            )
            .unwrap();
        let extractor = FakeExtractor {
            article: ExtractedArticle {
                title: Some("A title".to_string()),
                byline: None,
                markdown: ARTICLE.to_string(),
            },
        };
        let ctx = ctx_with(&config, &conn, &extractor);
        let job = claimed(&conn, &doc_id, Stage::Clean);

        let outcome = run(&ctx, &job).unwrap();

        assert_eq!(outcome, StageOutcome::Advance);
        let doc = control::get(&conn, &doc_id).unwrap().unwrap();
        assert_eq!(
            doc.clean_content_hash,
            Some(content_hash(&sanitize(ARTICLE)))
        );
        assert!(Path::new(&doc.clean_file_path.as_deref().unwrap_or("")).is_file());
        assert_eq!(doc.title.as_deref(), Some("A title"));
        assert_eq!(doc.language.as_deref(), Some("en"));
        assert!(doc.word_count.unwrap_or(0) >= 50);

        // The milestone lands through the §6 completion transaction.
        control::complete(&conn, Stage::Clean, &job, Completion::Chain, NOW).unwrap();
        assert_eq!(
            control::get(&conn, &doc_id).unwrap().unwrap().status,
            DocStatus::Cleaned
        );
    }

    #[tokio::test]
    async fn short_document_fails_the_quality_gate_without_chaining() {
        let dir = tempfile::tempdir().unwrap();
        let config = fixture_config(dir.path());
        let conn = boot();
        let doc_id = seed_doc(&conn, "a");
        let raw_path = write_raw(&config, &doc_id, "<html></html>");
        conn.raw()
            .execute(
                "UPDATE documents SET raw_file_path = ?1 WHERE doc_id = ?2",
                rusqlite::params![raw_path, doc_id],
            )
            .unwrap();
        let extractor = FakeExtractor {
            article: ExtractedArticle {
                title: None,
                byline: None,
                markdown: "too short".to_string(),
            },
        };
        let ctx = ctx_with(&config, &conn, &extractor);
        let job = claimed(&conn, &doc_id, Stage::Clean);

        let outcome = run(&ctx, &job).unwrap();

        assert_eq!(outcome, StageOutcome::Stop);
        let doc = control::get(&conn, &doc_id).unwrap().unwrap();
        assert_eq!(doc.status, DocStatus::FailedQuality);
        assert!(doc.error.as_deref().unwrap_or("").contains("word count"));
        // The job completes Done — nothing chains.
        control::complete(&conn, Stage::Clean, &job, Completion::Done, NOW).unwrap();
        let clean_job = conn
            .raw()
            .query_row(
                "SELECT status FROM jobs WHERE doc_id = ?1 AND stage = 'VECTORIZE'",
                [&doc_id],
                |r| r.get::<_, String>(0),
            )
            .map(|_: String| ())
            .or::<std::convert::Infallible>(Ok(()));
        assert!(clean_job.is_ok());
        let vectorize_jobs: i64 = conn
            .raw()
            .query_row(
                "SELECT count(*) FROM jobs WHERE doc_id = ?1 AND stage = 'VECTORIZE'",
                [&doc_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(vectorize_jobs, 0, "a rejected document chains nothing");
    }

    #[tokio::test]
    async fn non_target_language_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let config = fixture_config(dir.path());
        let conn = boot();
        let doc_id = seed_doc(&conn, "a");
        let raw_path = write_raw(&config, &doc_id, "<html></html>");
        conn.raw()
            .execute(
                "UPDATE documents SET raw_file_path = ?1 WHERE doc_id = ?2",
                rusqlite::params![raw_path, doc_id],
            )
            .unwrap();
        // Long French text: passes word count, fails the language gate.
        let french = "Le système de gestion de base de données relationnelle permet de stocker \
                      des informations structurées dans des tables reliées entre elles par des \\
                      clés étrangères très pratiques pour les applications modernes et anciennes.";
        let extractor = FakeExtractor {
            article: ExtractedArticle {
                title: None,
                byline: None,
                markdown: french.repeat(3),
            },
        };
        let ctx = ctx_with(&config, &conn, &extractor);
        let job = claimed(&conn, &doc_id, Stage::Clean);

        let outcome = run(&ctx, &job).unwrap();

        assert_eq!(outcome, StageOutcome::Stop);
        let doc = control::get(&conn, &doc_id).unwrap().unwrap();
        assert_eq!(doc.status, DocStatus::FailedQuality);
        assert!(doc.error.as_deref().unwrap_or("").contains("language"));
    }

    #[tokio::test]
    async fn duplicate_content_stops_without_chaining() {
        let dir = tempfile::tempdir().unwrap();
        let config = fixture_config(dir.path());
        let conn = boot();
        let original = seed_doc(&conn, "original");
        let duplicate = seed_doc(&conn, "dup");
        let dup_raw = write_raw(&config, &duplicate, "<html></html>");
        conn.raw()
            .execute(
                "UPDATE documents SET raw_file_path = ?1 WHERE doc_id = ?2",
                rusqlite::params![dup_raw, duplicate],
            )
            .unwrap();
        // The original already carries this exact content hash.
        conn.raw()
            .execute(
                "UPDATE documents SET clean_content_hash = ?1 WHERE doc_id = ?2",
                rusqlite::params![content_hash(&sanitize(ARTICLE)), original],
            )
            .unwrap();
        let extractor = FakeExtractor {
            article: ExtractedArticle {
                title: None,
                byline: None,
                markdown: ARTICLE.to_string(),
            },
        };
        let ctx = ctx_with(&config, &conn, &extractor);
        let job = claimed(&conn, &duplicate, Stage::Clean);
        assert_eq!(job.doc_id(), duplicate);

        let outcome = run(&ctx, &job).unwrap();

        assert_eq!(outcome, StageOutcome::Stop);
        let dup_doc = control::get(&conn, &duplicate).unwrap().unwrap();
        assert_eq!(
            dup_doc.clean_content_hash, None,
            "the duplicate stores nothing of its own"
        );
        let events: i64 = conn
            .raw()
            .query_row(
                "SELECT count(*) FROM stage_events WHERE doc_id = ?1 AND detail LIKE 'duplicate of%'",
                [&duplicate],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(events, 1, "the duplicate decision is auditable (§1.2.6)");
    }

    #[test]
    fn extractor_impl_extracts_title_content_and_strips_boilerplate() {
        let extractor = super::ReadabilityExtractor::new();
        let html = r"<html><head><title>The Page</title></head><body>
            <article><h1>Heading</h1><p>SQLite is an embedded database engine used
            everywhere from phones to browsers to airplanes today.</p></article>
            <nav>menu menu menu</nav></body></html>";
        let article = extractor.extract(html, "https://example.com/post").unwrap();
        assert_eq!(article.title.as_deref(), Some("The Page"));
        assert!(
            article.markdown.contains("embedded database engine"),
            "primary content survives: {:?}",
            article.markdown
        );
        assert!(
            !article.markdown.contains("menu menu menu"),
            "boilerplate removed"
        );
    }

    #[test]
    fn extractor_no_content_is_a_value_error_path() {
        let extractor = super::ReadabilityExtractor::new();
        let err = extractor
            .extract(
                "<html><body><script>x()</script></body></html>",
                "https://e.com/",
            )
            .unwrap_err();
        assert!(matches!(err, ExtractError::NoContent(_)), "got {err:?}");
        assert_eq!(err.class(), Class::Permanent);
    }

    #[test]
    fn sanitize_normalizes_strips_and_collapses() {
        // NFC: combining acute -> precomposed é.
        let nfc = sanitize("caf\u{65}\u{301}");
        assert_eq!(nfc, "caf\u{e9}\n");
        // data: URIs are stripped wholesale.
        let stripped = sanitize("![logo](data:image/svg+xml;base64,AAAA) text here");
        assert_eq!(stripped, " text here\n");
        // Blank-line runs collapse to two.
        let collapsed = sanitize("a\n\n\n\n\nb");
        assert_eq!(collapsed, "a\n\nb\n");
    }

    #[test]
    fn content_hash_is_stable_hex() {
        let h1 = content_hash("hello");
        let h2 = content_hash("hello");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
