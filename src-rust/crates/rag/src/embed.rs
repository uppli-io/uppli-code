// Embedding engine — wraps fastembed for local vector generation.

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::sync::{Mutex, OnceLock};
use tracing::info;

static EMBEDDER: OnceLock<Mutex<TextEmbedding>> = OnceLock::new();

pub struct Embedder;

impl Embedder {
    /// Initialize the embedding model (downloads on first use, ~22MB).
    pub fn init() {
        EMBEDDER.get_or_init(|| {
            info!("Loading embedding model (all-MiniLM-L6-v2)...");
            let model = TextEmbedding::try_new(
                InitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(true),
            )
            .expect("Failed to load embedding model");
            info!("Embedding model ready");
            Mutex::new(model)
        });
    }

    /// Embed a single text string. Returns a 384-dim vector.
    pub fn embed_one(text: &str) -> Vec<f32> {
        let lock = EMBEDDER.get().expect("Embedder not initialized");
        let mut model = lock.lock().unwrap();
        let results = model
            .embed(vec![text.to_string()], None)
            .expect("Embedding failed");
        results.into_iter().next().unwrap_or_default()
    }

    /// Embed multiple texts in batch.
    pub fn embed_batch(texts: &[String]) -> Vec<Vec<f32>> {
        let lock = EMBEDDER.get().expect("Embedder not initialized");
        let mut model = lock.lock().unwrap();
        model.embed(texts, None).expect("Batch embedding failed")
    }

    /// Check if the embedder is initialized.
    pub fn is_ready() -> bool {
        EMBEDDER.get().is_some()
    }
}

/// Cosine similarity between two vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_vectors() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 0.001);
    }

    #[test]
    fn cosine_orthogonal_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 0.001);
    }
}
