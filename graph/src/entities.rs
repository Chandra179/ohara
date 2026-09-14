//! Typed entity extraction and identity resolution for the graph process.
//!
//! The extractor is deterministic and local. It accepts explicit typed fields
//! such as `PERSON: Ada Lovelace`, then supplements them with lexical type cues
//! and well-known typed suffixes. It intentionally does not publish untyped
//! capitalized phrases as entities.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use sha2::{Digest, Sha256};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

const MAX_ENTITIES: usize = 32;
const MAX_NAME_WORDS: usize = 5;

const EXPLICIT_LABELS: &[(&str, EntityType)] = &[
    ("person", EntityType::Person),
    ("organization", EntityType::Organization),
    ("organisation", EntityType::Organization),
    ("place", EntityType::Location),
    ("location", EntityType::Location),
    ("event", EntityType::Event),
    ("concept", EntityType::Concept),
    ("product", EntityType::Product),
];

const ORGANIZATION_SUFFIXES: &[&str] = &[
    "agency",
    "bank",
    "company",
    "corp",
    "corporation",
    "foundation",
    "inc",
    "incorporated",
    "institute",
    "laboratory",
    "labs",
    "limited",
    "llc",
    "ltd",
    "media",
    "university",
];

const LOCATION_SUFFIXES: &[&str] = &[
    "city", "country", "county", "island", "lake", "mount", "province", "river", "state", "valley",
];

const EVENT_SUFFIXES: &[&str] = &[
    "agreement",
    "conference",
    "election",
    "festival",
    "games",
    "olympics",
    "summit",
    "treaty",
    "war",
];

const PERSON_PREFIXES: &[&str] = &["dr", "mr", "mrs", "ms", "prof", "president"];

const LOCATION_CUES: &[&str] = &["at", "from", "in", "near", "outside"];
const CONCEPT_CUES: &[&str] = &[
    "algorithm",
    "approach",
    "concept",
    "field",
    "method",
    "principle",
    "theory",
];
const PRODUCT_CUES: &[&str] = &[
    "app",
    "application",
    "browser",
    "framework",
    "library",
    "model",
    "platform",
    "product",
    "software",
];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum EntityType {
    Concept,
    Event,
    Location,
    Organization,
    Person,
    Product,
}

impl EntityType {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Concept => "CONCEPT",
            Self::Event => "EVENT",
            Self::Location => "LOCATION",
            Self::Organization => "ORGANIZATION",
            Self::Person => "PERSON",
            Self::Product => "PRODUCT",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Entity {
    pub(crate) kind: EntityType,
    pub(crate) name: String,
    pub(crate) normalized: String,
    pub(crate) aliases: Vec<String>,
}

impl Entity {
    pub(crate) fn id(&self) -> String {
        entity_id(self.kind, &self.normalized)
    }
}

#[derive(Clone, Debug)]
struct Mention {
    kind: EntityType,
    surface: String,
    canonical: Option<String>,
}

#[derive(Clone, Debug)]
struct Token {
    boundary_after: bool,
    value: String,
    lowercase: String,
    name_like: bool,
}

/// Extracts and resolves typed entities from one chunk.
pub(crate) fn extract(text: &str) -> Vec<Entity> {
    let tokens = tokenize(text);
    let mut mentions = explicit_mentions(text);
    mentions.extend(suffix_mentions(&tokens));
    mentions.extend(cue_mentions(&tokens));
    resolve(mentions)
}

fn explicit_mentions(text: &str) -> Vec<Mention> {
    let lowercase = text.to_ascii_lowercase();
    let mut mentions = Vec::new();
    for &(label, kind) in EXPLICIT_LABELS {
        let mut offset = 0;
        while let Some(relative) = lowercase[offset..].find(label) {
            let start = offset + relative;
            let end = start + label.len();
            let before_is_boundary = text[..start]
                .chars()
                .next_back()
                .is_none_or(|character| !character.is_alphanumeric() && character != '_');
            let Some(delimiter) = text[end..].find([':', '=']) else {
                offset = end;
                continue;
            };
            let delimiter_at = end + delimiter;
            let after_label = &text[end..delimiter_at];
            let after_is_boundary = after_label.chars().all(char::is_whitespace);
            if !before_is_boundary || !after_is_boundary {
                offset = end;
                continue;
            }

            let value_start = delimiter_at + 1;
            let value_end = text[value_start..]
                .find([';', '|', '\n', '.'])
                .map_or(text.len(), |relative| value_start + relative);
            let value = text[value_start..value_end].trim();
            if !value.is_empty() {
                let (canonical, aliases) = split_aliases(value);
                if let Some(canonical) = clean_name(canonical) {
                    mentions.push(Mention {
                        kind,
                        surface: canonical.clone(),
                        canonical: None,
                    });
                    mentions.extend(aliases.into_iter().filter_map(|alias| {
                        clean_name(&alias).map(|surface| Mention {
                            kind,
                            surface,
                            canonical: Some(canonical.clone()),
                        })
                    }));
                }
            }
            offset = value_end;
        }
    }
    mentions
}

fn split_aliases(value: &str) -> (&str, Vec<String>) {
    let lowercase = value.to_ascii_lowercase();
    for marker in ["also known as ", "aka "] {
        if let Some(index) = lowercase.find(marker) {
            let canonical = value[..index].trim_matches(trim_name_character);
            let alias = value[index + marker.len()..].trim_matches(trim_name_character);
            return (canonical, vec![alias.to_owned()]);
        }
    }

    if let Some(open) = value.find('(')
        && let Some(close) = value[open + 1..].find(')')
    {
        let close = open + close + 1;
        let canonical = value[..open].trim_matches(trim_name_character);
        let inside = value[open + 1..close].trim_matches(trim_name_character);
        let inside_lowercase = inside.to_ascii_lowercase();
        let alias = ["also known as ", "aka "]
            .iter()
            .find_map(|marker| {
                inside_lowercase
                    .strip_prefix(marker)
                    .map(|_| inside[marker.len()..].trim_matches(trim_name_character))
            })
            .unwrap_or(inside);
        if !alias.is_empty() {
            return (canonical, vec![alias.to_owned()]);
        }
    }
    (value, Vec::new())
}

fn suffix_mentions(tokens: &[Token]) -> Vec<Mention> {
    let mut mentions = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let kind = if ORGANIZATION_SUFFIXES.contains(&token.lowercase.as_str()) {
            Some(EntityType::Organization)
        } else if LOCATION_SUFFIXES.contains(&token.lowercase.as_str()) {
            Some(EntityType::Location)
        } else if EVENT_SUFFIXES.contains(&token.lowercase.as_str()) {
            Some(EntityType::Event)
        } else {
            None
        };
        let Some(kind) = kind else {
            continue;
        };
        let start = preceding_name_start(tokens, index);
        if start == index {
            continue;
        }
        let surface = tokens[start..=index]
            .iter()
            .map(|token| token.value.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        mentions.push(Mention {
            kind,
            surface,
            canonical: None,
        });
    }
    mentions
}

fn cue_mentions(tokens: &[Token]) -> Vec<Mention> {
    let mut mentions = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let (kind, accepts_lowercase) = if PERSON_PREFIXES.contains(&token.lowercase.as_str()) {
            (EntityType::Person, false)
        } else if LOCATION_CUES.contains(&token.lowercase.as_str()) {
            (EntityType::Location, false)
        } else if CONCEPT_CUES.contains(&token.lowercase.as_str()) {
            (EntityType::Concept, false)
        } else if PRODUCT_CUES.contains(&token.lowercase.as_str()) {
            (EntityType::Product, true)
        } else {
            continue;
        };
        let start = index + 1;
        let Some(end) = next_name_end(tokens, start, accepts_lowercase) else {
            continue;
        };
        let surface = tokens[start..end]
            .iter()
            .map(|token| token.value.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        mentions.push(Mention {
            kind,
            surface,
            canonical: None,
        });
    }
    mentions
}

fn preceding_name_start(tokens: &[Token], end: usize) -> usize {
    let mut start = end;
    while start > 0 && tokens[start - 1].name_like && end - start < MAX_NAME_WORDS - 1 {
        start -= 1;
    }
    start
}

fn next_name_end(tokens: &[Token], start: usize, accepts_lowercase: bool) -> Option<usize> {
    let first = tokens.get(start)?;
    if !first.name_like && !accepts_lowercase {
        return None;
    }
    let mut end = start + 1;
    while end < tokens.len()
        && end - start < MAX_NAME_WORDS
        && !tokens[end - 1].boundary_after
        && tokens[end].name_like
    {
        end += 1;
    }
    Some(end)
}

fn tokenize(text: &str) -> Vec<Token> {
    text.split_whitespace()
        .filter_map(|raw| {
            let value = raw.trim_matches(trim_name_character);
            if value.is_empty() {
                return None;
            }
            let lowercase = value.to_lowercase();
            let first_is_uppercase = value
                .chars()
                .find(|character| character.is_alphabetic())
                .is_some_and(char::is_uppercase);
            let is_acronym = value
                .chars()
                .filter(|character| character.is_alphabetic())
                .all(char::is_uppercase);
            let contains_digit = value.chars().any(|character| character.is_ascii_digit());
            Some(Token {
                boundary_after: raw.ends_with([';', '|', '.', ':']),
                value: value.to_owned(),
                lowercase,
                name_like: first_is_uppercase || is_acronym || contains_digit,
            })
        })
        .collect()
}

fn resolve(mentions: impl IntoIterator<Item = Mention>) -> Vec<Entity> {
    let mut groups: BTreeMap<(EntityType, String), (BTreeSet<String>, BTreeSet<String>)> =
        BTreeMap::new();
    for mention in mentions {
        let Some(surface) = clean_name(&mention.surface) else {
            continue;
        };
        let target = mention
            .canonical
            .as_deref()
            .and_then(normalize_name)
            .unwrap_or_else(|| normalize_name(&surface).unwrap_or_default());
        if target.is_empty() {
            continue;
        }
        let group = groups.entry((mention.kind, target)).or_default();
        if mention.canonical.is_none() {
            group.0.insert(surface);
        } else {
            group.1.insert(surface);
        }
    }

    groups
        .into_iter()
        .take(MAX_ENTITIES)
        .map(|((kind, normalized), (canonical_names, aliases))| {
            let name = canonical_names
                .into_iter()
                .next()
                .unwrap_or_else(|| normalized.clone());
            let aliases = aliases.into_iter().filter(|alias| alias != &name).collect();
            Entity {
                kind,
                name,
                normalized,
                aliases,
            }
        })
        .collect()
}

fn clean_name(value: &str) -> Option<String> {
    let cleaned = value.trim_matches(trim_name_character);
    (!cleaned.is_empty()).then(|| cleaned.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub(crate) fn normalize_name(value: &str) -> Option<String> {
    let normalized = value
        .nfkd()
        .filter(|character| !is_combining_mark(*character))
        .flat_map(char::to_lowercase)
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>();
    let normalized = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn entity_id(kind: EntityType, normalized: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(kind.as_str().as_bytes());
    digest.update(b":");
    digest.update(normalized.as_bytes());
    format!("entity-{}", hex(&digest.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn trim_name_character(character: char) -> bool {
    !character.is_alphanumeric() && !matches!(character, '\'' | '-' | '_')
}

#[cfg(test)]
mod tests {
    use super::{EntityType, extract, normalize_name, resolve};

    #[test]
    fn extracts_all_supported_types_from_structured_fields() {
        let entities = extract(
            "PERSON: Ada Lovelace (also known as Ada Byron); \
             ORGANIZATION: OpenAI (aka Open AI); \
             PLACE: São Paulo; EVENT: RustConf 2026; \
             CONCEPT: Knowledge Graph; PRODUCT: Ohara",
        );

        assert_eq!(entities.len(), 6);
        assert!(entities.iter().any(|entity| {
            entity.kind == EntityType::Person
                && entity.name == "Ada Lovelace"
                && entity.aliases == vec!["Ada Byron"]
        }));
        assert!(entities.iter().any(|entity| {
            entity.kind == EntityType::Organization
                && entity.name == "OpenAI"
                && entity.aliases == vec!["Open AI"]
        }));
        assert!(entities.iter().any(|entity| {
            entity.kind == EntityType::Location && entity.normalized == "sao paulo"
        }));
        assert!(
            entities
                .iter()
                .any(|entity| entity.kind == EntityType::Event)
        );
        assert!(
            entities
                .iter()
                .any(|entity| entity.kind == EntityType::Concept)
        );
        assert!(
            entities
                .iter()
                .any(|entity| entity.kind == EntityType::Product)
        );
    }

    #[test]
    fn typed_fields_extract_without_a_capitalized_phrase_baseline() {
        let entities = extract("organization: openai; product: ohara; in paris.");

        assert!(entities.iter().any(|entity| {
            entity.kind == EntityType::Organization && entity.normalized == "openai"
        }));
        assert!(
            entities.iter().any(|entity| {
                entity.kind == EntityType::Product && entity.normalized == "ohara"
            })
        );
        assert!(!entities.iter().any(|entity| entity.normalized == "paris"));
    }

    #[test]
    fn typed_lexical_cues_extract_people_and_suffix_entities() {
        let entities = extract(
            "Dr Ada Lovelace visited Acme Labs in São Paulo City during RustConf 2026 Conference.",
        );

        assert!(entities.iter().any(|entity| {
            entity.kind == EntityType::Person && entity.normalized == "ada lovelace"
        }));
        assert!(entities.iter().any(|entity| {
            entity.kind == EntityType::Organization && entity.normalized == "acme labs"
        }));
        assert!(entities.iter().any(|entity| {
            entity.kind == EntityType::Location && entity.normalized == "sao paulo city"
        }));
        assert!(entities.iter().any(|entity| {
            entity.kind == EntityType::Event && entity.normalized == "rustconf 2026 conference"
        }));
    }

    #[test]
    fn normalized_identity_handles_case_punctuation_and_diacritics() {
        assert_eq!(normalize_name(" José O'Hara "), Some("jose o hara".into()));
        assert_eq!(normalize_name("jose o'hara"), Some("jose o hara".into()));
    }

    #[test]
    fn resolution_keeps_same_name_different_types_separate() {
        let entities = resolve([
            super::Mention {
                kind: EntityType::Organization,
                surface: "Apple Inc.".into(),
                canonical: None,
            },
            super::Mention {
                kind: EntityType::Product,
                surface: "Apple".into(),
                canonical: None,
            },
        ]);

        assert_eq!(entities.len(), 2);
        assert_ne!(entities[0].id(), entities[1].id());
    }

    #[test]
    fn ignores_untyped_capitalized_phrases_and_empty_text() {
        assert!(extract("London Alice OpenAI").is_empty());
        assert!(extract(" ").is_empty());
    }
}
