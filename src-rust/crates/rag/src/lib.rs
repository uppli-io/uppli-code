// cc-rag: Local vector RAG for uppli-code.
//
// Uses fastembed (ONNX, local) to embed queries and search a vector store
// of tool knowledge (ast-grep patterns, etc.).
//
// Architecture:
//   - Chunks are stored as JSON + pre-computed embeddings in ~/.uppli/rag/
//   - At startup, embeddings are loaded into memory (Vec<f32>)
//   - Search = embed query → cosine similarity → top-K
//   - Model downloads automatically on first use (~22MB)

pub mod store;
pub mod embed;

pub use store::{Chunk, VectorStore};
pub use embed::Embedder;
