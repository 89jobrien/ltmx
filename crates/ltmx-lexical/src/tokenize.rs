//! Tokenization for lexical retrieval.
//!
//! Lowercases, splits on non-alphanumeric boundaries, and drops very short
//! tokens. Deliberately does not stem or remove stopwords: reconstructed
//! activity text is dense with identifiers (`vec0`, `colima`, `BAML`) where
//! stemming does more damage than it prevents.

/// Words too common to carry retrieval signal in activity text.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is", "it",
    "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there", "these",
    "they", "this", "to", "was", "will", "with",
];

/// Splits `text` into normalized index terms.
///
/// `_`, `-` and `.` are kept inside tokens so `sqlite-vec`, `MemxService` and
/// `store.rs` survive intact, but a token must still contain at least one
/// alphanumeric character -- otherwise a markdown rule (`---`) or an ellipsis
/// becomes an index term.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.')
        .map(str::to_lowercase)
        .filter(|token| token.len() >= 2 && !STOPWORDS.contains(&token.as_str()))
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_terms() {
        assert_eq!(tokenize("Docker Desktop"), vec!["docker", "desktop"]);
    }

    #[test]
    fn splits_on_punctuation() {
        assert_eq!(
            tokenize("chose vec0 over sqlite-vec"),
            vec!["chose", "vec0", "over", "sqlite-vec"]
        );
    }

    #[test]
    fn keeps_identifiers_intact() {
        assert_eq!(
            tokenize("call MemxService::insert"),
            vec!["call", "memxservice", "insert"]
        );
    }

    #[test]
    fn drops_stopwords() {
        assert_eq!(
            tokenize("the reason for the choice"),
            vec!["reason", "choice"]
        );
    }

    #[test]
    fn drops_single_character_tokens() {
        assert_eq!(tokenize("a b cd"), vec!["cd"]);
    }

    #[test]
    fn empty_text_yields_no_tokens() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("   ").is_empty());
    }

    #[test]
    fn punctuation_only_yields_no_tokens() {
        assert!(tokenize("--- /// ...").is_empty());
    }

    #[test]
    fn markdown_rules_and_ellipses_are_dropped() {
        // Front matter is common in notes; the rules themselves must not
        // become index terms.
        assert_eq!(tokenize("---\ntitle: x\n---"), vec!["title"]);
        assert!(tokenize("...").is_empty());
        assert!(tokenize("___").is_empty());
    }

    #[test]
    fn keeps_dotted_and_hyphenated_identifiers() {
        assert_eq!(
            tokenize("src/store.rs sqlite-vec"),
            vec!["src", "store.rs", "sqlite-vec"]
        );
    }
}
