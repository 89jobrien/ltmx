//! Combines a dense index (behind a deterministic fake embedder) with a
//! second ranked list via [`HybridRetriever`], so the example runs with no
//! network access and no real embedding backend.
//!
//! ```sh
//! cargo run -p ltmx-retrieve --example hybrid_search
//! ```

use chrono::{DateTime, Utc};
use ltmx_domain::{
    ActivityChunk, ActivityIndex, ChunkId, EmbedError, Embedder, LtmxError, Origin, ScoredChunk,
    SearchParams, SourceRef,
};
use ltmx_retrieve::{DenseIndex, HybridRetriever};
use std::sync::Arc;

/// Maps a chunk's first character to a fixed direction, so related chunks
/// embed close together without a real model.
#[derive(Clone)]
struct ToyEmbedder {
    dimensions: usize,
}

impl Embedder for ToyEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let slot = text
            .chars()
            .next()
            .map(|c| c as usize % self.dimensions.max(1))
            .unwrap_or(0);
        Ok((0..self.dimensions)
            .map(|i| if i == slot { 1.0 } else { 0.0 })
            .collect())
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn model_id(&self) -> String {
        "toy-embedder".to_string()
    }
}

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).expect("valid timestamp")
}

fn chunk(id: &str, text: &str) -> Arc<ActivityChunk> {
    ActivityChunk::new(
        ChunkId::new(id),
        text,
        Origin::Note,
        SourceRef::file(format!("/notes/{id}.md")),
        at(1_700_000_000),
    )
    .shared()
}

/// A trivial second retriever with a fixed ranking, standing in for a BM25
/// index so the example can show fusion without pulling in `ltmx-lexical`.
struct FixedRanking(Vec<Arc<ActivityChunk>>);

impl ActivityIndex for FixedRanking {
    fn upsert(&mut self, chunk: Arc<ActivityChunk>) -> Result<(), LtmxError> {
        self.0.push(chunk);
        Ok(())
    }
    fn upsert_all(&mut self, chunks: &[Arc<ActivityChunk>]) -> Result<(), LtmxError> {
        self.0.extend(chunks.iter().cloned());
        Ok(())
    }
    fn get(&self, id: &ChunkId) -> Result<Arc<ActivityChunk>, LtmxError> {
        self.0
            .iter()
            .find(|c| &c.id == id)
            .cloned()
            .ok_or_else(|| LtmxError::NotFound(id.clone()))
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn search(&self, _query: &str, params: &SearchParams) -> Result<Vec<ScoredChunk>, LtmxError> {
        Ok(self
            .0
            .iter()
            .filter(|c| params.admits(c))
            .take(params.top_k)
            .map(|chunk| ScoredChunk {
                chunk: chunk.clone(),
                score: 1.0,
            })
            .collect())
    }
}

fn main() {
    let mut dense = DenseIndex::new(ToyEmbedder { dimensions: 8 });
    let docker = chunk("docker", "docker desktop migration notes");
    let colima = chunk("colima", "colima replaced docker desktop");
    let gitea = chunk("gitea", "wired up the gitea runner");
    dense
        .upsert_all(&[docker.clone(), colima.clone(), gitea.clone()])
        .expect("upsert_all");

    let mut fixed = FixedRanking(Vec::new());
    fixed
        .upsert_all(&[colima, docker, gitea])
        .expect("upsert_all");

    let retriever = HybridRetriever::new(vec![
        &dense as &dyn ActivityIndex,
        &fixed as &dyn ActivityIndex,
    ]);

    let hits = retriever
        .search("docker", &SearchParams::default().with_top_k(3))
        .expect("search should succeed");

    println!(
        "fused ranking over {} child indexes:",
        retriever.index_count()
    );
    for hit in hits {
        println!("  {:>6.3}  {}", hit.score, hit.chunk.citation());
    }
}
