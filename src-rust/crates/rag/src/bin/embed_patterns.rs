// Pre-compute embeddings for the RAG pattern database.
// Usage: cargo run -p cc-rag --bin embed_patterns

use cc_rag::{Chunk, Embedder, VectorStore};
use std::path::PathBuf;

fn main() {
    tracing_subscriber::fmt::init();

    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("data/ast_grep_rag/patterns.json");
    if !input.exists() {
        eprintln!("Run data/generate_rag.py first to create patterns.json");
        std::process::exit(1);
    }

    println!("Loading patterns...");
    let content = std::fs::read_to_string(&input).expect("Failed to read patterns");
    let chunks: Vec<Chunk> = serde_json::from_str(&content).expect("Failed to parse");
    println!("Loaded {} chunks", chunks.len());

    println!("Initializing embedder (downloads model on first run)...");
    Embedder::init();

    let mut store = VectorStore::new();
    store.add_chunks(chunks);
    println!("Embeddings computed");

    store.save_default().expect("Failed to save");
    println!("Saved to {}", cc_rag::store::default_store_path().display());

    // Quick test
    println!("\n--- Test search ---");
    let results = store.search("raise ValueError error message python", Some("python"), Some("ast-grep"), 3);
    for r in &results {
        println!("  [{:.3}] {}", r.score, r.chunk.content.lines().next().unwrap_or(""));
    }
}
