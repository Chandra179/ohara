//! Pure text functions (§1.3): content hashing, language id, domain-dictionary
//! typo correction, tokenizer alignment, Unicode normalization, entity-surface
//! normalization and name similarity (§8 Stage 4). Zero I/O, no async — the
//! cheapest code to test exhaustively.

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// Nibble → hex char (keeps [`sha256_hex`] free of `expect`).
const HEX: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

/// Hex-encoded SHA-256 of `s` — the content-hash primitive behind §5's
/// `clean_content_hash`, §8's chunk dedup, and §3's deterministic chunk ids.
#[must_use]
pub fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(HEX[usize::from(byte >> 4)]);
        out.push(HEX[usize::from(byte & 0x0f)]);
    }
    out
}

/// Normalizes an entity surface form into its alias key (§8 Stage 4): NFC,
/// lowercase, trimmed, interior whitespace collapsed. The same transform
/// applies to extraction output and to candidate lookups, so exact alias hits
/// are order-insensitive.
#[must_use]
pub fn normalize_surface_form(surface: &str) -> String {
    surface
        .nfc()
        .collect::<String>()
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Normalized-edit-distance similarity in `0.0..=1.0` (§8 Stage 4's
/// normalized-name match): `1 - levenshtein(a, b) / max(len)`. Deterministic
/// and case-sensitive — callers normalize first. Identical strings score 1.0;
/// disjoint alphabets score 0.0.
#[must_use]
pub fn name_similarity(a: &str, b: &str) -> f64 {
    let (a_chars, b_chars): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let max_len = a_chars.len().max(b_chars.len());
    if max_len == 0 {
        return 1.0;
    }
    let distance = levenshtein_distance(&a_chars, &b_chars);
    let max_len = u32::try_from(max_len).unwrap_or(u32::MAX);
    1.0 - f64::from(distance) / f64::from(max_len)
}

/// Classic Levenshtein distance over two char slices (the workspace, kept out of
/// [`name_similarity`]'s way).
fn levenshtein_distance(a: &[char], b: &[char]) -> u32 {
    let width = b.len() + 1;
    let mut prev: Vec<usize> = (0..width).collect();
    let mut curr = vec![0usize; width];
    for (i, a_ch) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, b_ch) in b.iter().enumerate() {
            curr[j + 1] = (curr[j] + 1)
                .min(prev[j + 1] + 1)
                .min(prev[j] + usize::from(a_ch != b_ch));
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    u32::try_from(prev[width - 1]).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp
)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn sha256_is_deterministic_and_input_sensitive() {
        assert_eq!(sha256_hex("abc"), sha256_hex("abc"));
        assert_ne!(sha256_hex("abc"), sha256_hex("abd"));
    }

    #[test]
    fn surface_forms_normalize_to_the_alias_key() {
        assert_eq!(normalize_surface_form("  Barack  Obama "), "barack obama");
        assert_eq!(normalize_surface_form("BARACK OBAMA"), "barack obama");
        assert_eq!(
            normalize_surface_form("PostgreSQL\u{00A0}SQL"), // NBSP collapses
            "postgresql sql"
        );
        assert_eq!(normalize_surface_form(""), "");
    }

    #[test]
    fn name_similarity_measures_edit_distance() {
        assert_eq!(name_similarity("sqlite", "sqlite"), 1.0);
        assert!(name_similarity("sqlite", "sqlit") > 0.8);
        assert!(name_similarity("postgresql", "postgres") > 0.7);
        assert_eq!(name_similarity("abc", "xyz"), 0.0);
        assert_eq!(name_similarity("", ""), 1.0);
        assert_eq!(name_similarity("", "abc"), 0.0);
    }
}
