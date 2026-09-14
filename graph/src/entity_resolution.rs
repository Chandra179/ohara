//! Labeled entity-resolution threshold measurement.
//!
//! This module measures approximate candidate merges without changing the
//! graph process's production identity rule. Exact normalized identities and
//! explicit aliases remain the only automatic production merges until a
//! measured approximate policy is deliberately adopted.

use std::collections::BTreeSet;
use std::fs;

use serde::Deserialize;
use strsim::jaro_winkler;

use crate::entities::{EntityType, normalize_name};

const SCHEMA_VERSION: u8 = 1;
const SCORE_EPSILON: f64 = f64::EPSILON * 4.0;

#[derive(Debug, Deserialize)]
struct Fixture {
    schema_version: u8,
    selected_threshold: f64,
    thresholds: Vec<f64>,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    id: String,
    reason: String,
    left: Mention,
    right: Mention,
    expected_same: bool,
}

#[derive(Debug, Deserialize)]
struct Mention {
    document_id: String,
    entity_type: String,
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    threshold: f64,
    precision: f64,
    recall: f64,
    f1: f64,
    false_merges: u32,
    missed_merges: u32,
}

/// Runs a versioned entity-resolution benchmark and prints its report.
pub(super) fn run(path: &str) -> Result<(), String> {
    let fixture: Fixture = serde_json::from_str(
        &fs::read_to_string(path).map_err(|error| format!("read {path}: {error}"))?,
    )
    .map_err(|error| format!("decode {path}: {error}"))?;
    validate(&fixture)?;

    let measurements = fixture
        .thresholds
        .iter()
        .copied()
        .map(|threshold| measure(&fixture.cases, threshold))
        .collect::<Vec<_>>();
    let selected = measurements
        .iter()
        .find(|measurement| {
            (measurement.threshold - fixture.selected_threshold).abs() <= SCORE_EPSILON
        })
        .ok_or_else(|| {
            format!(
                "selected threshold {} is not present in fixture thresholds",
                fixture.selected_threshold
            )
        })?;
    let best = measurements
        .iter()
        .max_by(|left, right| compare_measurements(left, right))
        .ok_or_else(|| "entity-resolution fixture has no thresholds".to_owned())?;
    if selected.f1 + SCORE_EPSILON < best.f1 {
        return Err(format!(
            "selected threshold {:.2} is not optimal: F1 {:.3}; best threshold {:.2} has F1 {:.3}",
            selected.threshold, selected.f1, best.threshold, best.f1
        ));
    }

    println!(
        "Entity-resolution benchmark (schema v{}):",
        fixture.schema_version
    );
    println!("threshold  precision  recall  f1     false_merges  missed_merges");
    for measurement in &measurements {
        println!(
            "{:<9.2}  {:<9.3}  {:<6.3}  {:<6.3} {:<13} {}",
            measurement.threshold,
            measurement.precision,
            measurement.recall,
            measurement.f1,
            measurement.false_merges,
            measurement.missed_merges,
        );
    }
    println!(
        "selected threshold: {:.2} (precision {:.3}, recall {:.3}, F1 {:.3})",
        selected.threshold, selected.precision, selected.recall, selected.f1
    );
    println!(
        "unresolved behavior: candidates below {:.2} remain separate; exact normalized identities and explicit aliases remain deterministic merges",
        selected.threshold
    );
    Ok(())
}

fn validate(fixture: &Fixture) -> Result<(), String> {
    if fixture.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "unsupported entity-resolution schema version {}; expected {SCHEMA_VERSION}",
            fixture.schema_version
        ));
    }
    if !fixture.selected_threshold.is_finite() || !(0.0..=1.0).contains(&fixture.selected_threshold)
    {
        return Err("selected threshold must be finite and between 0 and 1".to_owned());
    }
    if fixture.thresholds.is_empty()
        || fixture
            .thresholds
            .iter()
            .any(|threshold| !threshold.is_finite() || !(0.0..=1.0).contains(threshold))
    {
        return Err("thresholds must be finite values between 0 and 1".to_owned());
    }
    let mut thresholds = BTreeSet::new();
    if fixture
        .thresholds
        .iter()
        .any(|threshold| !thresholds.insert(threshold.to_bits()))
    {
        return Err("thresholds must be unique".to_owned());
    }
    let mut ids = BTreeSet::new();
    if fixture.cases.is_empty() {
        return Err("entity-resolution fixture has no cases".to_owned());
    }
    for case in &fixture.cases {
        if case.id.is_empty() || !ids.insert(&case.id) {
            return Err(format!("case id is empty or duplicated: {}", case.id));
        }
        if case.reason.is_empty() {
            return Err(format!("case {} has no reason", case.id));
        }
        validate_mention(&case.id, &case.left)?;
        validate_mention(&case.id, &case.right)?;
    }
    Ok(())
}

fn validate_mention(case_id: &str, mention: &Mention) -> Result<(), String> {
    if mention.document_id.is_empty() || mention.name.trim().is_empty() {
        return Err(format!("case {case_id} has an incomplete mention"));
    }
    parse_entity_type(&mention.entity_type)
        .map(|_| ())
        .ok_or_else(|| format!("case {case_id} has an unknown entity type"))?;
    for alias in &mention.aliases {
        if alias.trim().is_empty() {
            return Err(format!("case {case_id} has an empty alias"));
        }
    }
    Ok(())
}

fn measure(cases: &[Case], threshold: f64) -> Metrics {
    let mut true_positive = 0_u32;
    let mut false_positive = 0_u32;
    let mut false_merges = 0_u32;
    let mut missed_merges = 0_u32;
    for case in cases {
        let merged = resolution_score(&case.left, &case.right)
            .is_some_and(|score| score + SCORE_EPSILON >= threshold);
        match (merged, case.expected_same) {
            (true, true) => true_positive += 1,
            (true, false) => {
                false_positive += 1;
                false_merges += 1;
            }
            (false, true) => missed_merges += 1,
            (false, false) => {}
        }
    }
    let precision = if true_positive + false_positive == 0 {
        0.0
    } else {
        f64::from(true_positive) / f64::from(true_positive + false_positive)
    };
    let recall = if true_positive + missed_merges == 0 {
        0.0
    } else {
        f64::from(true_positive) / f64::from(true_positive + missed_merges)
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    Metrics {
        threshold,
        precision,
        recall,
        f1,
        false_merges,
        missed_merges,
    }
}

fn compare_measurements(left: &Metrics, right: &Metrics) -> std::cmp::Ordering {
    left.f1
        .total_cmp(&right.f1)
        .then_with(|| left.precision.total_cmp(&right.precision))
        .then_with(|| right.threshold.total_cmp(&left.threshold))
}

fn resolution_score(left: &Mention, right: &Mention) -> Option<f64> {
    let left_type = parse_entity_type(&left.entity_type)?;
    let right_type = parse_entity_type(&right.entity_type)?;
    if left_type != right_type {
        return None;
    }
    let left_name = normalize_name(&left.name)?;
    let right_name = normalize_name(&right.name)?;
    if left_name == right_name
        || left
            .aliases
            .iter()
            .filter_map(|alias| normalize_name(alias))
            .any(|alias| alias == right_name)
        || right
            .aliases
            .iter()
            .filter_map(|alias| normalize_name(alias))
            .any(|alias| alias == left_name)
    {
        return Some(1.0);
    }
    Some(jaro_winkler(&left_name, &right_name))
}

fn parse_entity_type(value: &str) -> Option<EntityType> {
    match value.to_ascii_uppercase().as_str() {
        "CONCEPT" => Some(EntityType::Concept),
        "EVENT" => Some(EntityType::Event),
        "LOCATION" | "PLACE" => Some(EntityType::Location),
        "ORGANIZATION" | "ORGANISATION" => Some(EntityType::Organization),
        "PERSON" => Some(EntityType::Person),
        "PRODUCT" => Some(EntityType::Product),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Fixture, compare_measurements, measure, validate};

    fn fixture() -> Result<Fixture, String> {
        serde_json::from_str(include_str!(
            "../../docs/architecture/fixtures/entity-resolution-v1.json"
        ))
        .map_err(|error| format!("entity-resolution fixture should decode: {error}"))
    }

    #[test]
    fn fixture_is_valid_and_selected_threshold_is_best() -> Result<(), String> {
        let fixture = fixture()?;
        validate(&fixture)?;
        let selected = measure(&fixture.cases, fixture.selected_threshold);
        let best = fixture
            .thresholds
            .iter()
            .copied()
            .map(|threshold| measure(&fixture.cases, threshold))
            .max_by(compare_measurements)
            .ok_or_else(|| "fixture should have thresholds".to_owned())?;
        assert!((selected.threshold - best.threshold).abs() <= super::SCORE_EPSILON);
        assert!(selected.precision >= 0.8);
        assert!(selected.recall >= 0.8);
        Ok(())
    }

    #[test]
    fn type_mismatch_never_becomes_a_merge_candidate() -> Result<(), String> {
        let fixture = fixture()?;
        let case = fixture
            .cases
            .iter()
            .find(|case| case.id == "same-name-different-type")
            .ok_or_else(|| "type collision case is missing".to_owned())?;
        assert_eq!(measure(std::slice::from_ref(case), 0.0).false_merges, 0);
        assert_eq!(measure(std::slice::from_ref(case), 0.0).missed_merges, 0);
        Ok(())
    }
}
