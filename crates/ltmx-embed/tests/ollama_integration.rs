//! Integration test for the Ollama adapter against a real local server.
//!
//! Ignored by default: CI must not depend on Ollama running. Run it with
//!
//! ```sh
//! cargo test -p ltmx-embed -- --ignored
//! ```
//!
//! It is the only test that proves the hand-rolled HTTP/1.1 client actually
//! speaks to Ollama -- unit tests can only prove it builds a well-formed body.

#![cfg(feature = "ollama")]

use ltmx_domain::Embedder;
use ltmx_embed::{NOMIC_DIMENSIONS, OllamaEmbedder};

fn embedder() -> OllamaEmbedder {
    OllamaEmbedder::new("nomic-embed-text")
}

/// Dot product of two vectors.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Cosine similarity.
///
/// Ollama returns **un-normalized** `nomic-embed-text` vectors -- a raw dot
/// product comes back around 200, not 1 -- so any dense index built on this
/// backend must normalize before comparing.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let magnitude = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot(a, b) / (magnitude(a) * magnitude(b))
}

#[test]
#[ignore = "requires a local Ollama server"]
fn embeds_text_at_the_declared_width() {
    let embedder = embedder();
    let vector = embedder
        .embed("vec0 sqlite vector search")
        .expect("embed should succeed");
    assert_eq!(vector.len(), NOMIC_DIMENSIONS);
    assert_eq!(vector.len(), embedder.dimensions());
}

#[test]
#[ignore = "requires a local Ollama server"]
fn embeds_related_text_closer_than_unrelated_text() {
    let embedder = embedder();
    let anchor = embedder
        .embed("why did we switch to colima")
        .expect("anchor");
    let related = embedder
        .embed("colima replaced docker desktop on this machine")
        .expect("related");
    let unrelated = embedder
        .embed("sourdough starter feeding schedule")
        .expect("unrelated");

    assert!(
        cosine(&anchor, &related) > cosine(&anchor, &unrelated),
        "semantically related text must score higher: {} vs {}",
        cosine(&anchor, &related),
        cosine(&anchor, &unrelated)
    );
}

#[test]
#[ignore = "requires a local Ollama server"]
fn batch_preserves_order_and_width() {
    let embedder = embedder();
    let texts = vec![
        "first".to_string(),
        "second about colima".to_string(),
        "third".to_string(),
    ];
    let vectors = embedder.embed_batch(&texts).expect("batch should succeed");
    assert_eq!(vectors.len(), texts.len());
    for vector in &vectors {
        assert_eq!(vector.len(), NOMIC_DIMENSIONS);
    }
    assert!(
        vectors[0] != vectors[1],
        "distinct texts must produce distinct vectors"
    );
    assert!(
        cosine(&vectors[0], &vectors[1]) < 0.999,
        "unrelated one-word texts should not be near-identical: {}",
        cosine(&vectors[0], &vectors[1])
    );
}

#[test]
#[ignore = "requires a local Ollama server"]
fn unreachable_host_surfaces_a_transport_error() {
    let embedder = OllamaEmbedder::new("nomic-embed-text").with_host("127.0.0.1:1");
    let err = embedder.embed("anything").expect_err("port 1 must refuse");
    assert!(err.to_string().contains("unreachable"), "got {err}");
}
