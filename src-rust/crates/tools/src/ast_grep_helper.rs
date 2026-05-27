// AstGrepHelper tool: RAG-powered pattern lookup for AstEdit.
//
// The model calls this tool to get ast-grep pattern examples BEFORE
// writing its own pattern. Returns the most relevant examples from
// the vector store based on what the model wants to do.

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

pub struct AstGrepHelperTool;

#[derive(Debug, Deserialize)]
struct HelperInput {
    /// Describe what you want to do in natural language.
    query: String,
    /// Programming language.
    #[serde(default = "default_lang")]
    language: String,
}

fn default_lang() -> String {
    "python".to_string()
}

#[async_trait]
impl Tool for AstGrepHelperTool {
    fn name(&self) -> &str {
        "AstGrepHelper"
    }

    fn description(&self) -> &str {
        "Get ast-grep pattern examples for code editing. Describe what you \
         want to do and get the right pattern syntax. Call this before using \
         AstEdit to get the correct pattern format."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "What you want to do. Example: 'change raise ValueError message', \
                        'add argument to re.compile', 'make comparison case insensitive'"
                },
                "language": {
                    "type": "string",
                    "description": "Programming language (default: python)",
                    "default": "python"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        let params: HelperInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        if !cc_rag::Embedder::is_ready() {
            return ToolResult::error(
                "RAG not initialized. The embedding model is still loading.".to_string(),
            );
        }

        let store = match cc_rag::VectorStore::load_default() {
            Ok(s) => s,
            Err(e) => return ToolResult::error(format!("RAG store not available: {}", e)),
        };

        let results =
            store.search_owned(&params.query, Some(&params.language), Some("ast-grep"), 5);

        if results.is_empty() {
            return ToolResult::success(
                "No matching patterns found. Basic rules:\n\
                 - $VAR matches one AST node\n\
                 - $$$VAR matches zero or more nodes\n\
                 - NEVER match string literal contents — use $$$ARGS\n\
                 - Indentation in rewrite is automatic"
                    .to_string(),
            );
        }

        let mut output = String::new();
        for (chunk, score) in &results {
            output.push_str(&format!(
                "\n[{}] (relevance: {:.0}%)\n{}\n",
                chunk.language,
                score * 100.0,
                chunk.content,
            ));
        }
        output.push_str(
            "\nRules: $VAR=one node, $$$VAR=multiple. \
             NEVER match string literal contents — use wildcards.\n",
        );

        ToolResult::success(output)
    }
}
