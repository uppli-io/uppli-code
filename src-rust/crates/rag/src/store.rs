// Vector store — in-memory search over pre-embedded chunks.
//
// Chunks are stored as JSON with pre-computed embeddings in ~/.uppli/rag/.
// At startup, loaded into memory. Search = embed query → cosine similarity → top-K.
//
// Chunk design for optimal retrieval:
//   - Each chunk is a self-contained unit of knowledge (~100-200 tokens)
//   - The "search_text" field is what gets embedded (intent + context)
//   - The "content" field is what gets returned to the model (pattern + rewrite)
//   - Chunks are language-tagged for filtering

use crate::embed::{cosine_similarity, Embedder};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, info};

/// A single chunk of knowledge in the vector store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    /// Unique identifier.
    pub id: String,
    /// Category (e.g., "ast-grep", "edit-pattern", "tool-usage").
    pub category: String,
    /// Programming language ("python", "javascript", "rust", "any").
    pub language: String,
    /// Text used for embedding/search (the query side).
    /// This is what the vector search matches against.
    /// Should describe the INTENT: "add argument to function call",
    /// "modify raise ValueError message", "wrap statement in if check".
    pub search_text: String,
    /// Content returned to the model (the answer side).
    /// Should be actionable: the pattern, rewrite, and a brief explanation.
    pub content: String,
    /// Pre-computed embedding vector (384 dims for MiniLM).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedding: Vec<f32>,
}

/// In-memory vector store.
pub struct VectorStore {
    chunks: Vec<Chunk>,
}

impl Default for VectorStore {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self { chunks: Vec::new() }
    }

    /// Load chunks from a JSON file. Embeddings must be pre-computed.
    pub fn load_from_file(path: &PathBuf) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
        let chunks: Vec<Chunk> = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))?;
        let count = chunks.len();
        let with_embeddings = chunks.iter().filter(|c| !c.embedding.is_empty()).count();
        info!(
            count,
            with_embeddings,
            path = %path.display(),
            "Vector store loaded"
        );
        Ok(Self { chunks })
    }

    /// Load from the default path (~/.uppli/rag/vector_store.json).
    pub fn load_default() -> Result<Self, String> {
        let path = default_store_path();
        if !path.exists() {
            return Err(format!("No vector store at {}", path.display()));
        }
        Self::load_from_file(&path)
    }

    /// Add chunks and compute their embeddings.
    pub fn add_chunks(&mut self, mut chunks: Vec<Chunk>) {
        // Embed any chunks that don't have embeddings yet.
        let needs_embedding: Vec<usize> = chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| c.embedding.is_empty())
            .map(|(i, _)| i)
            .collect();

        if !needs_embedding.is_empty() && Embedder::is_ready() {
            let texts: Vec<String> = needs_embedding
                .iter()
                .map(|&i| chunks[i].search_text.clone())
                .collect();
            let embeddings = Embedder::embed_batch(&texts);
            for (text_idx, &chunk_idx) in needs_embedding.iter().enumerate() {
                if text_idx < embeddings.len() {
                    chunks[chunk_idx].embedding = embeddings[text_idx].clone();
                }
            }
            debug!(
                count = needs_embedding.len(),
                "Computed embeddings for new chunks"
            );
        }

        self.chunks.extend(chunks);
    }

    /// Save the store to a JSON file (with embeddings for fast reload).
    pub fn save_to_file(&self, path: &PathBuf) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create dir: {}", e))?;
        }
        let json = serde_json::to_string_pretty(&self.chunks)
            .map_err(|e| format!("Failed to serialize: {}", e))?;
        std::fs::write(path, json)
            .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
        info!(
            count = self.chunks.len(),
            path = %path.display(),
            "Vector store saved"
        );
        Ok(())
    }

    /// Save to default path.
    pub fn save_default(&self) -> Result<(), String> {
        self.save_to_file(&default_store_path())
    }

    /// Search for the most relevant chunks.
    pub fn search(
        &self,
        query: &str,
        language: Option<&str>,
        category: Option<&str>,
        max_results: usize,
    ) -> Vec<SearchResult<'_>> {
        if self.chunks.is_empty() || !Embedder::is_ready() {
            return Vec::new();
        }

        let query_embedding = Embedder::embed_one(query);

        let mut results: Vec<SearchResult> = self
            .chunks
            .iter()
            .filter(|c| {
                // Language filter: match if specified, or if chunk is "any"
                if let Some(lang) = language {
                    if c.language != lang && c.language != "any" {
                        return false;
                    }
                }
                // Category filter
                if let Some(cat) = category {
                    if c.category != cat {
                        return false;
                    }
                }
                // Must have embedding
                !c.embedding.is_empty()
            })
            .map(|chunk| {
                let score = cosine_similarity(&query_embedding, &chunk.embedding);
                SearchResult { chunk, score }
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(max_results);

        // Filter out low-relevance results (below 0.3 similarity)
        results.retain(|r| r.score > 0.3);

        results
    }

    /// Search and return owned results (for use across function boundaries).
    pub fn search_owned(
        &self,
        query: &str,
        language: Option<&str>,
        category: Option<&str>,
        max_results: usize,
    ) -> Vec<(Chunk, f32)> {
        self.search(query, language, category, max_results)
            .into_iter()
            .map(|r| (r.chunk.clone(), r.score))
            .collect()
    }

    /// Number of chunks in the store.
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// Is the store empty?
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

/// A search result with relevance score.
pub struct SearchResult<'a> {
    pub chunk: &'a Chunk,
    pub score: f32,
}

/// Format search results for display to the model.
pub fn format_results(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. [{}] {}\n",
            i + 1,
            r.chunk.language,
            r.chunk.content
        ));
    }
    out
}

/// Default store path.
pub fn default_store_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".uppli")
        .join("rag")
        .join("vector_store.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_chunk(id: &str, search: &str, content: &str) -> Chunk {
        Chunk {
            id: id.to_string(),
            category: "ast-grep".to_string(),
            language: "python".to_string(),
            search_text: search.to_string(),
            content: content.to_string(),
            embedding: Vec::new(),
        }
    }

    #[test]
    fn empty_store_returns_no_results() {
        let store = VectorStore::new();
        assert!(store.is_empty());
    }

    #[test]
    fn add_chunks_increases_count() {
        let mut store = VectorStore::new();
        store.add_chunks(vec![sample_chunk("1", "test query", "test content")]);
        assert_eq!(store.len(), 1);
    }
}
