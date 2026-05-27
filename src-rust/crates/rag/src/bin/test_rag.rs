// Test the RAG with queries that simulate what the model would ask.

use cc_rag::{Embedder, VectorStore};

fn main() {
    tracing_subscriber::fmt::init();

    println!("Loading RAG...");
    Embedder::init();
    let store = VectorStore::load_default().expect("Load store");
    println!("Store: {} chunks\n", store.len());

    let tests = vec![
        // The exact cases that failed in the benchmark
        ("raise ValueError error message python", "python"),
        ("change exception message in raise statement", "python"),
        (
            "modify raise ValueError to show all required columns",
            "python",
        ),
        ("add re.IGNORECASE flag to re.compile", "python"),
        ("make string comparison case insensitive", "python"),
        ("add line after function call", "python"),
        ("set attribute to None after delete", "python"),
        // General queries
        ("replace method call with different method", "python"),
        ("match function definition", "rust"),
        ("replace var with const", "javascript"),
        ("wrap code in try catch", "javascript"),
        ("match error check pattern", "go"),
        // Edge cases
        ("how to handle strings with braces in pattern", "any"),
        ("how to match multiple arguments", "any"),
    ];

    for (query, lang) in tests {
        let results = store.search_owned(query, Some(lang), Some("ast-grep"), 3);
        println!("Q: \"{}\" [{}]", query, lang);
        if results.is_empty() {
            println!("  (no results)\n");
        } else {
            for (chunk, score) in &results {
                let first_line = chunk.content.lines().next().unwrap_or("");
                println!("  [{:.0}%] {}", score * 100.0, first_line);
            }
            println!();
        }
    }
}
