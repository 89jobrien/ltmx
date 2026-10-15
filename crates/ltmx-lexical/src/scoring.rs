//! BM25 ranking function.
//!
//! Pure arithmetic, no index state, so it is testable on its own and reusable
//! by any adapter that maintains its own postings.

/// BM25 free parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bm25Params {
    /// Term-frequency saturation. Higher values let a term count more.
    pub k1: f32,
    /// Length normalization strength. `0.0` disables it.
    pub b: f32,
}

impl Default for Bm25Params {
    /// The standard Okapi BM25 defaults.
    fn default() -> Self {
        Self { k1: 1.2, b: 0.75 }
    }
}

/// Scores `query_terms` against a single document.
///
/// `document` maps a term to its frequency in that document. `doc_freq` is the
/// number of documents in the corpus containing the term, `corpus_size` the
/// total document count, `doc_len` this document's token count, and
/// `avg_len` the mean token count across the corpus.
///
/// Returns `0.0` when no query term appears in the document or the corpus is
/// empty.
pub fn bm25(
    query_terms: &[String],
    document: &std::collections::HashMap<String, u32>,
    doc_len: u32,
    avg_len: f32,
    corpus_size: f32,
    doc_freq: impl Fn(&str) -> u32,
    params: Bm25Params,
) -> f32 {
    if corpus_size <= 0.0 || avg_len <= 0.0 || doc_len == 0 {
        return 0.0;
    }

    let mut score = 0.0;
    for term in query_terms {
        let Some(&tf) = document.get(term) else {
            continue;
        };
        let df = doc_freq(term) as f32;
        // Robertson/Sparck-Jones IDF with the +1 guard so a term present in
        // every document cannot produce a negative weight.
        let idf = (((corpus_size - df + 0.5) / (df + 0.5)) + 1.0).ln();
        let tf_norm = tf as f32
            / (tf as f32 + params.k1 * (1.0 - params.b + params.b * doc_len as f32 / avg_len));
        score += idf * (tf_norm * (params.k1 + 1.0) / (tf_norm + params.k1));
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn doc(pairs: &[(&str, u32)]) -> HashMap<String, u32> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
    }

    fn always_one(_: &str) -> u32 {
        1
    }

    fn terms(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn empty_corpus_scores_zero() {
        let score = bm25(
            &terms(&["a"]),
            &doc(&[("a", 1)]),
            1,
            1.0,
            0.0,
            always_one,
            Bm25Params::default(),
        );
        assert_eq!(score, 0.0);
    }

    #[test]
    fn zero_average_length_scores_zero() {
        let score = bm25(
            &terms(&["a"]),
            &doc(&[("a", 1)]),
            1,
            0.0,
            1.0,
            always_one,
            Bm25Params::default(),
        );
        assert_eq!(score, 0.0);
    }

    #[test]
    fn absent_term_scores_zero() {
        let score = bm25(
            &terms(&["docker"]),
            &doc(&[("colima", 3)]),
            10,
            10.0,
            5.0,
            always_one,
            Bm25Params::default(),
        );
        assert_eq!(score, 0.0);
    }

    #[test]
    fn matching_term_scores_positive() {
        let score = bm25(
            &terms(&["docker"]),
            &doc(&[("docker", 2)]),
            10,
            10.0,
            5.0,
            always_one,
            Bm25Params::default(),
        );
        assert!(score > 0.0);
    }

    #[test]
    fn rare_term_outweighs_common_term() {
        let params = Bm25Params::default();
        let document = doc(&[("rare", 1), ("common", 1)]);
        let common_only = bm25(&terms(&["common"]), &document, 10, 10.0, 5.0, |_| 5, params);
        let rare = bm25(&terms(&["rare"]), &document, 10, 10.0, 5.0, |_| 1, params);
        assert!(
            rare > common_only,
            "rare {rare} should beat common {common_only}"
        );
    }

    #[test]
    fn higher_term_frequency_scores_higher() {
        let params = Bm25Params::default();
        let low = bm25(
            &terms(&["docker"]),
            &doc(&[("docker", 1)]),
            10,
            10.0,
            5.0,
            |_| 2,
            params,
        );
        let high = bm25(
            &terms(&["docker"]),
            &doc(&[("docker", 5)]),
            10,
            10.0,
            5.0,
            |_| 2,
            params,
        );
        assert!(high > low, "high {high} should beat low {low}");
    }

    #[test]
    fn shorter_document_scores_higher_for_same_frequency() {
        let params = Bm25Params::default();
        let document = doc(&[("docker", 1)]);
        let short = bm25(&terms(&["docker"]), &document, 5, 20.0, 5.0, |_| 1, params);
        let long = bm25(
            &terms(&["docker"]),
            &document,
            100,
            20.0,
            5.0,
            |_| 1,
            params,
        );
        assert!(short > long, "short {short} should beat long {long}");
    }

    #[test]
    fn b_zero_disables_length_normalization() {
        let params = Bm25Params { k1: 1.2, b: 0.0 };
        let document = doc(&[("docker", 1)]);
        let short = bm25(&terms(&["docker"]), &document, 5, 20.0, 5.0, |_| 1, params);
        let long = bm25(
            &terms(&["docker"]),
            &document,
            100,
            20.0,
            5.0,
            |_| 1,
            params,
        );
        assert!((short - long).abs() < f32::EPSILON);
    }

    #[test]
    fn empty_query_scores_zero() {
        let score = bm25(
            &[],
            &doc(&[("docker", 1)]),
            10,
            10.0,
            5.0,
            always_one,
            Bm25Params::default(),
        );
        assert_eq!(score, 0.0);
    }
}
