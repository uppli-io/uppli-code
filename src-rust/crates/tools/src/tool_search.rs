// ToolSearch: intelligent tool discovery + RAG vector search.
//
// Three modes:
//   1. "select:ToolName" → full expertise for a specific tool
//   2. "keyword query"   → ranked tool recommendations
//   3. Automatic RAG     → if query relates to code editing/ast-grep,
//                           also searches the vector store for pattern examples

use crate::tool_expertise;
use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

pub struct ToolSearchTool;

#[derive(Debug, Deserialize)]
struct ToolSearchInput {
    query: String,
    #[serde(default = "default_max")]
    max_results: usize,
}

fn default_max() -> usize {
    5
}

/// Keywords that trigger RAG vector search for ast-grep patterns.
const RAG_TRIGGER_KEYWORDS: &[&str] = &[
    "ast-grep", "ast_grep", "astedit", "pattern", "rewrite",
    "structural", "raise", "exception", "function call", "method",
    "import", "class", "return", "assign", "replace code",
    "modify code", "edit code", "change code", "add line",
    "insert after", "wrap with", "indentation",
];

fn should_search_rag(query: &str) -> bool {
    let q = query.to_lowercase();
    RAG_TRIGGER_KEYWORDS.iter().any(|kw| q.contains(kw))
}

/// Extract language hint from query (e.g., "python raise ValueError" → "python").
fn detect_language(query: &str) -> Option<&'static str> {
    let q = query.to_lowercase();
    let langs = [
        ("python", "python"), ("py ", "python"), (".py", "python"),
        ("javascript", "javascript"), ("js ", "javascript"), (".js", "javascript"),
        ("typescript", "typescript"), ("ts ", "typescript"), (".ts", "typescript"),
        ("rust", "rust"), (".rs", "rust"),
        ("go ", "go"), ("golang", "go"), (".go", "go"),
        ("java", "java"), (".java", "java"),
        ("ruby", "ruby"), (".rb", "ruby"),
        ("c++", "cpp"), ("cpp", "cpp"),
    ];
    for (keyword, lang) in &langs {
        if q.contains(keyword) {
            return Some(lang);
        }
    }
    None
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        "ToolSearch"
    }

    fn description(&self) -> &str {
        "Find the right tool or get pattern examples for code editing. \
         Describe what you need: 'ast-grep raise ValueError python' returns \
         pattern examples. 'modify file' returns tool recommendations. \
         Use 'select:ToolName' for full tool details."
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
                    "description": "Describe what you need. Examples: \
                        'ast-grep raise ValueError python', \
                        'modify python file', \
                        'select:AstEdit'"
                },
                "max_results": {
                    "type": "number",
                    "description": "Maximum results (default: 5)",
                    "default": 5
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        let params: ToolSearchInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let query = params.query.trim();
        let max = params.max_results.min(20);

        // Mode 1: "select:ToolName" — full expertise
        if let Some(names_str) = query.strip_prefix("select:").map(str::trim) {
            let mut output = String::new();
            let mut missing = Vec::new();

            for name in names_str.split(',').map(str::trim) {
                if let Some(entry) = tool_expertise::get(name) {
                    output.push_str(&tool_expertise::format_full(entry));
                    output.push('\n');
                } else {
                    missing.push(name.to_string());
                }
            }

            if output.is_empty() {
                return ToolResult::success(format!(
                    "No tools found: {}.",
                    missing.join(", ")
                ));
            }
            if !missing.is_empty() {
                output.push_str(&format!("\nNot found: {}", missing.join(", ")));
            }
            return ToolResult::success(output);
        }

        // Mode 2+3: tool search + optional RAG
        let mut output = String::new();

        // RAG vector search for ast-grep patterns
        if should_search_rag(query) {
            let lang = detect_language(query);
            if let Ok(store) = cc_rag::VectorStore::load_default() {
                let rag_results = store.search_owned(query, lang, Some("ast-grep"), max);
                if !rag_results.is_empty() {
                    output.push_str("Pattern examples:\n");
                    for (chunk, score) in &rag_results {
                        output.push_str(&format!(
                            "\n  [{}] (relevance: {:.0}%)\n  {}\n",
                            chunk.language,
                            score * 100.0,
                            chunk.content.replace('\n', "\n  "),
                        ));
                    }
                    output.push_str("\nRules: $VAR=one node, $$$VAR=multiple. NEVER match string literals.\n");
                }
            }
        }

        // Tool expertise search
        let tool_results = tool_expertise::search(query, max);
        if !tool_results.is_empty() {
            if !output.is_empty() {
                output.push_str("\n---\n");
            }
            output.push_str(&tool_expertise::format_search_results(&tool_results));
        }

        if output.is_empty() {
            output = format!("No results for '{}'. Try different keywords.", query);
        }

        ToolResult::success(output)
    }
}
