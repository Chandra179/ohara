//! Deterministic ranking, latency, and embedding metrics.

use crate::{Result, check};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

/// Return a nearest-rank percentile from millisecond samples.
pub(crate) fn percentile(samples: &[f64], percentage: f64) -> Result<f64> {
    check(
        !samples.is_empty(),
        "cannot calculate a percentile without samples",
    )?;
    let mut ordered = samples.to_vec();
    ordered.sort_by(f64::total_cmp);
    let target = percentage.clamp(0.0, 100.0) / 100.0;
    let count = count_as_f64(ordered.len());
    let rank = ordered
        .iter()
        .enumerate()
        .position(|(index, _)| count_as_f64(index + 1) / count >= target)
        .unwrap_or(ordered.len() - 1);
    Ok(ordered[rank])
}

/// Mirror the production deterministic test embedding.
pub(crate) fn deterministic_embedding(text: &str) -> Vec<f64> {
    (0usize..384)
        .map(|index| {
            let mut digest = Sha256::new();
            digest.update(text.as_bytes());
            digest.update(index.to_le_bytes());
            let bytes = digest.finalize();
            let value = u16::from_le_bytes([bytes[0], bytes[1]]);
            f64::from(value) / 65_535.0 * 2.0 - 1.0
        })
        .collect()
}

/// Calculate cosine similarity, returning zero for a zero vector.
pub(crate) fn cosine(left: &[f64], right: &[f64]) -> f64 {
    let left_norm = left.iter().map(|value| value * value).sum::<f64>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f64>().sqrt();
    if left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }
    left.iter().zip(right).map(|(a, b)| a * b).sum::<f64>() / (left_norm * right_norm)
}

/// Calculate recall at a cutoff using positive relevance grades.
pub(crate) fn recall_at_k(
    ranked_ids: &[String],
    relevance: &[(String, i64)],
    cutoff: usize,
) -> f64 {
    let relevant: HashSet<&str> = relevance
        .iter()
        .filter(|(_, grade)| *grade > 0)
        .map(|(id, _)| id.as_str())
        .collect();
    if relevant.is_empty() {
        return 0.0;
    }
    let retrieved: HashSet<&str> = ranked_ids.iter().take(cutoff).map(String::as_str).collect();
    count_as_f64(relevant.intersection(&retrieved).count()) / count_as_f64(relevant.len())
}

/// Calculate mean reciprocal rank for the first positive relevance result.
pub(crate) fn mean_reciprocal_rank(ranked_ids: &[String], relevance: &[(String, i64)]) -> f64 {
    ranked_ids
        .iter()
        .position(|id| {
            relevance
                .iter()
                .any(|(known, grade)| known == id && *grade > 0)
        })
        .map_or(0.0, |position| 1.0 / count_as_f64(position + 1))
}

/// Calculate normalized discounted cumulative gain at a cutoff.
pub(crate) fn ndcg_at_k(ranked_ids: &[String], relevance: &[(String, i64)], cutoff: usize) -> f64 {
    fn gain(grade: i64) -> f64 {
        let exponent = u32::try_from(grade).map_or(0, |value| value.min(31));
        f64::from(2_u32.pow(exponent) - 1)
    }
    fn discounted_gain(grades: impl Iterator<Item = i64>, cutoff: usize) -> f64 {
        grades
            .take(cutoff)
            .enumerate()
            .map(|(rank, grade)| gain(grade) / count_as_f64(rank + 2).log2())
            .sum()
    }
    let actual = discounted_gain(
        ranked_ids.iter().map(|id| {
            relevance
                .iter()
                .find(|(known, _)| known == id)
                .map_or(0, |(_, grade)| *grade)
        }),
        cutoff,
    );
    let mut ideal_grades: Vec<i64> = relevance.iter().map(|(_, grade)| *grade).collect();
    ideal_grades.sort_unstable_by(|left, right| right.cmp(left));
    let ideal = discounted_gain(ideal_grades.into_iter(), cutoff);
    if ideal == 0.0 { 0.0 } else { actual / ideal }
}

/// Run regression checks for the ranking metric implementations.
pub(crate) fn metric_regression_tests() -> Result<()> {
    let relevance = vec![
        ("best".to_owned(), 3),
        ("good".to_owned(), 2),
        ("noise".to_owned(), 0),
    ];
    let ranked = ["noise", "good", "best"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    check(
        (recall_at_k(&ranked, &relevance, 1) - 0.0).abs() < f64::EPSILON,
        "recall@1 regression test failed",
    )?;
    check(
        (recall_at_k(&ranked, &relevance, 3) - 1.0).abs() < f64::EPSILON,
        "recall@3 regression test failed",
    )?;
    check(
        (mean_reciprocal_rank(&ranked, &relevance) - 0.5).abs() < f64::EPSILON,
        "MRR regression test failed",
    )?;
    let expected = ((3.0 / 3.0_f64.log2()) + (7.0 / 4.0_f64.log2()))
        / ((7.0 / 2.0_f64.log2()) + (3.0 / 3.0_f64.log2()));
    check(
        (ndcg_at_k(&ranked, &relevance, 3) - expected).abs() < 1e-12,
        "nDCG regression test failed",
    )
}

fn count_as_f64(value: usize) -> f64 {
    u32::try_from(value).map_or(f64::MAX, f64::from)
}

#[cfg(test)]
mod tests {
    use super::{mean_reciprocal_rank, ndcg_at_k, percentile, recall_at_k};

    #[test]
    fn percentile_uses_nearest_rank() {
        assert!(
            (percentile(&[4.0, 1.0, 3.0, 2.0], 50.0).unwrap_or(0.0) - 2.0).abs() < f64::EPSILON
        );
        assert!(
            (percentile(&[4.0, 1.0, 3.0, 2.0], 95.0).unwrap_or(0.0) - 4.0).abs() < f64::EPSILON
        );
    }

    #[test]
    fn ranking_metrics_match_fixture_expectations() {
        let relevance = vec![
            ("best".to_owned(), 3),
            ("good".to_owned(), 2),
            ("noise".to_owned(), 0),
        ];
        let ranked = ["noise", "good", "best"].map(str::to_owned);
        assert!((recall_at_k(&ranked, &relevance, 1) - 0.0).abs() < f64::EPSILON);
        assert!((recall_at_k(&ranked, &relevance, 3) - 1.0).abs() < f64::EPSILON);
        assert!((mean_reciprocal_rank(&ranked, &relevance) - 0.5).abs() < f64::EPSILON);
        assert!(ndcg_at_k(&ranked, &relevance, 3) > 0.6);
    }
}
