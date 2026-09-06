//! Stage 3a — Chunk (§8): header-aware split → recursive fallback with overlap →
//! breadcrumbs, budgeted in the embedder's tokenizer. Tables and fenced code
//! blocks are atomic; the budget is measured in the *embedder's tokenizer* (§4),
//! so the split functions take a token counter rather than counting words.

/// §8 Stage 3 budget: ≤ 512 tokens in the embedder's tokenizer, breadcrumb
/// included. The embedder itself truncates at the same 512 (§4), so chunks that
/// pass this gate are never truncated in practice.
pub(super) const CHUNK_BUDGET_TOKENS: usize = 512;

/// Overlap between recursive-split windows of one over-budget section (§8):
/// 10–15% of the content budget — 12% sits mid-range.
const OVERLAP_PERCENT: usize = 12;

/// One chunk ready for the registry + embedding (§5 `chunks` columns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Section content — what `chunks.text` stores.
    pub text: String,
    /// The exact string to embed: breadcrumb prefix + blank line + `text` (§8).
    pub embed_text: String,
    /// Document title + header path ("Title > H1 > H2"); the root is the title.
    pub header_path: String,
    /// 0-based position within the document.
    pub seq: usize,
}

/// Splits `markdown` into chunks under `budget` tokens as measured by `tokens`
/// (the embedder's tokenizer — §4), prefixing every chunk with a breadcrumb
/// built from `title` and the section's heading path.
///
/// Pure function: `tokens` is injected so tests can use a cheap approximation
/// and the stage passes [`crate::pipeline::Embedder::count_tokens`].
pub fn chunk_document(
    markdown: &str,
    title: &str,
    budget: usize,
    tokens: &dyn Fn(&str) -> usize,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut header_path = title.to_string();
    let mut section = String::new();

    for block in &split_top_level(markdown) {
        if let Some((level, text)) = heading(block) {
            // A heading starts a new section: flush what came before it. The
            // breadcrumb replaces the title with the heading chain (§8); the
            // heading line itself lives in the breadcrumb, its body stays here.
            push_section(&mut chunks, &section, &header_path, budget, tokens);
            section.clear();
            let depth = usize::from(level); // 1-based depth of the heading
            let path: Vec<&str> = header_path
                .split(" > ")
                .take(depth) // truncate deeper paths when a shallower heading repeats
                .collect();
            header_path = if depth == 1 {
                text.to_string()
            } else {
                let mut rebuilt: Vec<String> = path[..depth.saturating_sub(1).min(path.len())]
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect();
                rebuilt.push(text.to_string());
                rebuilt.join(" > ")
            };
            if let Some((_, body)) = block.split_once('\n') {
                section.push_str(body);
            }
        } else {
            section.push_str(block);
        }
    }
    push_section(&mut chunks, &section, &header_path, budget, tokens);
    finish(chunks)
}

/// Flushes one section into `chunks`, recursing into the split ladder when the
/// section (breadcrumb included) is over budget (§8).
fn push_section(
    chunks: &mut Vec<Chunk>,
    section: &str,
    header_path: &str,
    budget: usize,
    tokens: &dyn Fn(&str) -> usize,
) {
    let trimmed = section.trim();
    if trimmed.is_empty() {
        return;
    }
    let breadcrumb = format!("{header_path}\n\n");
    let content_budget = budget.saturating_sub(tokens(&breadcrumb));
    if content_budget == 0 {
        // Degenerate (breadcrumb alone fills the budget): emit the section as
        // one chunk anyway — the embedder truncates, never drops (§8).
        chunks.push(Chunk {
            text: trimmed.to_string(),
            embed_text: format!("{breadcrumb}{trimmed}"),
            header_path: header_path.to_string(),
            seq: 0,
        });
        return;
    }
    if tokens(trimmed) <= content_budget {
        chunks.push(Chunk {
            text: trimmed.to_string(),
            embed_text: format!("{breadcrumb}{trimmed}"),
            header_path: header_path.to_string(),
            seq: 0,
        });
        return;
    }
    for (text, header) in recursive_split(trimmed, header_path, content_budget, tokens) {
        chunks.push(Chunk {
            embed_text: format!("{header}\n\n{text}"),
            text,
            header_path: header.clone(),
            seq: 0,
        });
    }
}

/// Numbers the finished chunks; takes ownership so the caller's Vec is consumed.
fn finish(mut chunks: Vec<Chunk>) -> Vec<Chunk> {
    for (seq, chunk) in chunks.iter_mut().enumerate() {
        chunk.seq = seq;
    }
    chunks
}

/// Splits markdown into top-level blocks: headings (with their following
/// content), and everything else. Fenced code blocks are kept whole (§8).
fn split_top_level(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;
    let mut fence_marker = String::new();

    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if in_fence {
            current.push_str(line);
            current.push('\n');
            if trimmed.starts_with(&fence_marker) {
                in_fence = false;
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush(&mut blocks, &mut current);
            fence_marker = trimmed
                .chars()
                .take_while(|c| *c == '`' || *c == '~')
                .collect();
            in_fence = true;
            current.push_str(line);
            current.push('\n');
            continue;
        }
        if is_heading(trimmed) {
            flush(&mut blocks, &mut current);
            current.push_str(line);
            current.push('\n');
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    flush(&mut blocks, &mut current);
    blocks
}

fn flush(blocks: &mut Vec<String>, current: &mut String) {
    if current.trim().is_empty() {
        current.clear();
    } else {
        blocks.push(std::mem::take(current));
    }
}

fn is_heading(line: &str) -> bool {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    (1..=3).contains(&hashes) && line[hashes..].starts_with(' ')
}

/// `(level 1..=3, text)` for an ATX heading line, else [`None`].
fn heading(block: &str) -> Option<(u8, &str)> {
    let line = block.lines().next()?;
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if !(1..=3).contains(&hashes) || !line[hashes..].starts_with(' ') {
        return None;
    }
    let text = line[hashes..].trim();
    Some((u8::try_from(hashes).ok()?, text))
}

/// §8's split ladder: `\n\n` → `\n` → sentence, packing the pieces into
/// budget-sized windows with ~12% overlap. Tables and fenced code blocks inside
/// the section are atomic units — never split mid-block.
fn recursive_split<'a>(
    text: &'a str,
    header_path: &'a str,
    budget: usize,
    tokens: &dyn Fn(&str) -> usize,
) -> Vec<(String, String)> {
    let units = atomic_units(text);
    pack(units, header_path, budget, tokens)
}

/// Breaks `text` into the smallest atomic pieces the ladder allows: paragraphs,
/// then lines for oversized paragraphs, then sentences for oversized lines.
/// Tables (runs of `|`/`+---` lines) and fenced code stay single units.
fn atomic_units(text: &str) -> Vec<String> {
    let mut units = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;
    let mut in_table = false;

    for line in text.lines() {
        let trimmed = line.trim_start();
        if in_fence {
            current.push_str(line);
            current.push('\n');
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_fence = false;
                units.push(std::mem::take(&mut current));
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush(&mut units, &mut current);
            in_fence = true;
            current.push_str(line);
            current.push('\n');
            continue;
        }
        let is_table_line =
            trimmed.starts_with('|') || (trimmed.starts_with('+') && trimmed.contains('-'));
        if in_table && !is_table_line {
            in_table = false;
            units.push(std::mem::take(&mut current));
        }
        if is_table_line && !in_table {
            flush(&mut units, &mut current);
            in_table = true;
        }
        if is_table_line {
            current.push_str(line);
            current.push('\n');
            continue;
        }
        if trimmed.is_empty() {
            if !current.trim().is_empty() {
                units.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    if in_table && !current.trim().is_empty() {
        units.push(std::mem::take(&mut current));
    } else {
        flush(&mut units, &mut current);
    }
    // Ladder: any paragraph still holding multiple lines splits on lines; any
    // single line over budget splits on sentences (done at pack time — the
    // packer is the only place that knows the budget).
    units = split_long_paragraphs(units);
    units
}

fn split_long_paragraphs(units: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(units.len());
    for unit in units {
        let head = unit.trim_start();
        let atomic = head.starts_with("```")
            || head.starts_with('~')
            || head.starts_with('|')
            || (head.starts_with('+') && head.contains('-'));
        if unit.lines().count() > 1 && !atomic {
            for line in unit.lines() {
                if !line.trim().is_empty() {
                    out.push(format!("{line}\n"));
                }
            }
        } else {
            out.push(unit);
        }
    }
    out
}

/// Packs units into budget-sized windows; a unit larger than the whole budget
/// (an oversized table or code block, §8) becomes its own window, split on
/// sentences only when it is prose.
fn pack(
    units: Vec<String>,
    header_path: &str,
    budget: usize,
    tokens: &dyn Fn(&str) -> usize,
) -> Vec<(String, String)> {
    let mut pieces: Vec<String> = Vec::new();
    for unit in units {
        let t = unit.trim();
        if t.is_empty() {
            continue;
        }
        if tokens(t) <= budget {
            pieces.push(t.to_string());
            continue;
        }
        if t.starts_with("```") || t.starts_with('|') || t.starts_with('+') {
            // Atomic (§8): oversized table/fence = its own chunk, unsplit.
            pieces.push(t.to_string());
            continue;
        }
        for sentence in sentences(t) {
            if !sentence.trim().is_empty() {
                pieces.push(sentence);
            }
        }
    }

    let overlap_budget = budget.saturating_mul(OVERLAP_PERCENT) / 100;
    let mut windows: Vec<(String, String)> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_tokens = 0usize;

    for piece in pieces {
        let piece_tokens = tokens(&piece);
        if piece_tokens > budget {
            // A single sentence over budget: its own window (embedder
            // truncation is the documented backstop, §8).
            flush_window(&mut windows, &mut current, &mut current_tokens, header_path);
            windows.push((piece, header_path.to_string()));
            continue;
        }
        if current_tokens + piece_tokens > budget && !current.is_empty() {
            flush_window(&mut windows, &mut current, &mut current_tokens, header_path);
            // Overlap: carry the tail of the previous window (§8, 10–15%).
            let mut carried = 0usize;
            let mut overlap: Vec<String> = Vec::new();
            for prev in current.iter().rev() {
                let t = tokens(prev);
                if carried + t > overlap_budget {
                    break;
                }
                carried += t;
                overlap.push(prev.clone());
            }
            overlap.reverse();
            current = overlap;
            current_tokens = carried;
        }
        current_tokens += piece_tokens;
        current.push(piece);
    }
    flush_window(&mut windows, &mut current, &mut current_tokens, header_path);
    windows
}

fn flush_window(
    windows: &mut Vec<(String, String)>,
    current: &mut Vec<String>,
    current_tokens: &mut usize,
    header_path: &str,
) {
    if current.is_empty() {
        return;
    }
    windows.push((current.join("\n\n"), header_path.to_string()));
    current.clear();
    *current_tokens = 0;
}

/// Sentence split on `.`, `!`, `?` followed by whitespace/EOL; abbreviations
/// are not special-cased — over-splitting only shrinks atomic pieces, and the
/// packer re-glues them up to the budget.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'.' || *b == b'!' || *b == b'?' {
            let next = bytes.get(i + 1);
            if next.is_none_or(u8::is_ascii_whitespace) {
                out.push(text[start..=i].trim().to_string());
                start = i + 1;
            }
        }
    }
    if start < text.len() {
        let tail = text[start..].trim();
        if !tail.is_empty() {
            out.push(tail.to_string());
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    /// Whitespace-approximation counter — deterministic and cheap; the real
    /// tokenizer alignment is exercised by the embedder's own tests.
    fn words(s: &str) -> usize {
        s.split_whitespace().count()
    }

    #[test]
    fn single_section_under_budget_is_one_chunk() {
        let md = "plain text without headings";
        let chunks = chunk_document(md, "Title", 512, &words);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].seq, 0);
        assert_eq!(chunks[0].text, md);
        assert_eq!(chunks[0].embed_text, format!("Title\n\n{md}"));
        assert_eq!(chunks[0].header_path, "Title");
    }

    #[test]
    fn headings_build_breadcrumbs_and_reset_sections() {
        let md = "intro\n\n# Alpha\nalpha body\n\n## Beta\nbeta body\n\n## Gamma\ngamma body\n\n# Delta\ndelta body";
        let chunks = chunk_document(md, "Doc", 512, &words);
        let paths: Vec<&str> = chunks.iter().map(|c| c.header_path.as_str()).collect();
        assert_eq!(
            paths,
            ["Doc", "Alpha", "Alpha > Beta", "Alpha > Gamma", "Delta"]
        );
        assert!(chunks[1].embed_text.starts_with("Alpha\n\nalpha body"));
        // h2 after h2 truncates the path to depth 2
        assert!(
            chunks[3]
                .embed_text
                .starts_with("Alpha > Gamma\n\ngamma body")
        );
    }

    #[test]
    fn over_budget_section_recursively_splits_with_overlap() {
        let para = |n: usize| format!("sentence {n} with some words to fill budget. ");
        let md: String = std::iter::repeat_n(para(1), 40).collect();
        let chunks = chunk_document(&md, "T", 60, &words);
        assert!(chunks.len() > 1, "expected multiple windows");
        // Budget respected on every chunk
        for chunk in &chunks {
            assert!(words(&chunk.text) <= 60, "len {}", words(&chunk.text));
        }
        // Overlap: window n+1 shares its opening with window n's tail
        let second = &chunks[1].text;
        let first_tail = chunks[0]
            .text
            .rsplit_once('\n')
            .map_or(&*chunks[0].text, |(_, t)| t);
        assert!(
            second.contains(first_tail),
            "no overlap: {first_tail:?} vs {second:?}"
        );
    }

    #[test]
    fn tables_and_code_fences_are_atomic() {
        let table = "| a | b |\n|---|---|\n| 1 | 2 |";
        let md = format!("# H\n{table}\n\n```rust\nfn main() {{}}\n```\n");
        let chunks = chunk_document(&md, "T", 512, &words);
        assert!(chunks.iter().any(|c| c.text.contains("| 1 | 2 |")));
        assert!(chunks.iter().any(|c| c.text.contains("fn main()")));
        for chunk in &chunks {
            assert!(
                !chunk.text.contains("| a | b |\nfn main"),
                "table and code fused into one non-atomic unit"
            );
        }
    }

    #[test]
    fn oversized_table_becomes_its_own_chunk_unsplit() {
        let mut table = String::from("| a | b |\n|---|---|\n");
        for i in 0..200 {
            let _ = write!(table, "| x{i} | y{i} |");
            table.push('\n');
        }
        let chunks = chunk_document(&table, "T", 30, &words);
        assert_eq!(chunks.len(), 1, "table never split mid-block (§8)");
        assert!(chunks[0].text.contains("| x199 | y199 |"));
    }

    #[test]
    fn budget_includes_the_breadcrumb() {
        // Breadcrumb eats into the budget: content must shrink accordingly.
        // Two sentences so the ladder can re-pack them under the shrunken
        // budget (an unbreakable single sentence is the documented backstop).
        let md = "one two three four five. six seven eight nine ten.";
        let big_title = "T".repeat(8);
        let chunks = chunk_document(md, &big_title, 12, &words);
        assert_eq!(
            chunks.len(),
            1,
            "breadcrumb + content within budget: one chunk"
        );
        let chunks = chunk_document(md, "one two three", 12, &words);
        assert!(
            chunks.len() >= 2,
            "breadcrumb + content over budget must split"
        );
    }

    #[test]
    fn empty_and_whitespace_only_documents_yield_no_chunks() {
        assert!(chunk_document("", "T", 512, &words).is_empty());
        assert!(chunk_document("  \n\n \t ", "T", 512, &words).is_empty());
    }

    #[test]
    fn sentences_split_on_terminal_punctuation() {
        let s = sentences("First one. Second one! Third one? tail");
        assert_eq!(s, ["First one.", "Second one!", "Third one?", "tail"]);
    }
}
