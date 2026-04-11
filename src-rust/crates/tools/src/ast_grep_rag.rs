// ast-grep RAG — pattern knowledge base with TF-IDF matching.
//
// Provides ast-grep pattern examples to the model BEFORE it writes a pattern.
// The model calls ToolSearch("ast-grep ...") and gets relevant examples.
// No external dependencies — in-memory TF-IDF over a JSON pattern database.
//
// The database is loaded from ~/.uppli/rag/ast_grep_patterns.json if it exists,
// otherwise falls back to the built-in examples. Users can add their own patterns.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use tracing::debug;

// ---------------------------------------------------------------------------
// Pattern entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstGrepPattern {
    /// What this pattern does (natural language).
    pub description: String,
    /// The ast-grep pattern.
    pub pattern: String,
    /// The ast-grep rewrite (if applicable).
    #[serde(default)]
    pub rewrite: String,
    /// Programming language.
    pub language: String,
    /// Tags for matching (e.g., "raise", "exception", "add-argument").
    #[serde(default)]
    pub tags: Vec<String>,
}

// ---------------------------------------------------------------------------
// In-memory index with TF-IDF scoring
// ---------------------------------------------------------------------------

struct RagIndex {
    patterns: Vec<AstGrepPattern>,
    /// Inverted index: term → list of (pattern_index, term_frequency).
    inverted: HashMap<String, Vec<(usize, f32)>>,
    /// Document count for IDF.
    doc_count: f32,
}

impl RagIndex {
    fn build(patterns: Vec<AstGrepPattern>) -> Self {
        let doc_count = patterns.len() as f32;
        let mut inverted: HashMap<String, Vec<(usize, f32)>> = HashMap::new();

        for (i, pat) in patterns.iter().enumerate() {
            // Build a document from all searchable fields.
            let doc = format!(
                "{} {} {} {} {}",
                pat.description,
                pat.pattern,
                pat.rewrite,
                pat.language,
                pat.tags.join(" ")
            )
            .to_lowercase();

            // Count term frequencies.
            let mut term_counts: HashMap<String, usize> = HashMap::new();
            for term in tokenize(&doc) {
                *term_counts.entry(term).or_default() += 1;
            }
            let total_terms = term_counts.values().sum::<usize>() as f32;

            for (term, count) in term_counts {
                let tf = count as f32 / total_terms;
                inverted.entry(term).or_default().push((i, tf));
            }
        }

        Self {
            patterns,
            inverted,
            doc_count,
        }
    }

    fn search(&self, query: &str, language: &str, max: usize) -> Vec<(&AstGrepPattern, f32)> {
        let query_terms = tokenize(&query.to_lowercase());
        let mut scores: HashMap<usize, f32> = HashMap::new();

        for term in &query_terms {
            if let Some(postings) = self.inverted.get(term.as_str()) {
                // IDF: log(N / df)
                let idf = (self.doc_count / postings.len() as f32).ln();
                for &(doc_idx, tf) in postings {
                    *scores.entry(doc_idx).or_default() += tf * idf;
                }
            }
        }

        // Boost matching language.
        let lang_lower = language.to_lowercase();
        for (idx, score) in scores.iter_mut() {
            let pat = &self.patterns[*idx];
            if pat.language.to_lowercase() == lang_lower || pat.language == "any" {
                *score *= 1.5;
            }
        }

        let mut results: Vec<(usize, f32)> = scores.into_iter().collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(max);

        results
            .into_iter()
            .map(|(idx, score)| (&self.patterns[idx], score))
            .collect()
    }
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
        .filter(|s| s.len() >= 2)
        .map(|s| s.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

static RAG_INDEX: OnceLock<RagIndex> = OnceLock::new();

fn get_index() -> &'static RagIndex {
    RAG_INDEX.get_or_init(|| {
        let patterns = load_patterns();
        debug!(count = patterns.len(), "ast-grep RAG index loaded");
        RagIndex::build(patterns)
    })
}

fn load_patterns() -> Vec<AstGrepPattern> {
    // Try user file first.
    let user_path = rag_file_path();
    if user_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&user_path) {
            if let Ok(patterns) = serde_json::from_str::<Vec<AstGrepPattern>>(&content) {
                if !patterns.is_empty() {
                    debug!(path = %user_path.display(), count = patterns.len(), "Loaded user RAG");
                    return patterns;
                }
            }
        }
    }

    // Fallback to built-in.
    builtin_patterns()
}

fn rag_file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".uppli")
        .join("rag")
        .join("ast_grep_patterns.json")
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Search for ast-grep patterns matching a query.
pub fn search(query: &str, language: &str, max: usize) -> Vec<(&'static AstGrepPattern, f32)> {
    get_index().search(query, language, max)
}

/// Format search results for display to the model.
pub fn format_results(results: &[(&AstGrepPattern, f32)]) -> String {
    if results.is_empty() {
        return String::new();
    }

    let mut out = String::from("ast-grep pattern examples:\n");
    for (pat, _score) in results {
        out.push_str(&format!(
            "\n  [{}] {}\n    pattern: {}\n",
            pat.language, pat.description, pat.pattern,
        ));
        if !pat.rewrite.is_empty() {
            out.push_str(&format!("    rewrite: {}\n", pat.rewrite));
        }
    }
    out.push_str("\nRules: $VAR=one node, $$$VAR=multiple, NEVER match string literals.\n");
    out
}

/// Core rules — always included when AstEdit is mentioned.
pub fn core_rules() -> &'static str {
    "ast-grep: $VAR=one node, $$$VAR=multiple nodes. \
     NEVER match string literal contents — use wildcards. \
     Indentation is automatic."
}

// ---------------------------------------------------------------------------
// Built-in patterns
// ---------------------------------------------------------------------------

fn builtin_patterns() -> Vec<AstGrepPattern> {
    vec![
        // ---- Python ----
        p("Match any raise statement", "raise $EXCEPTION($$$ARGS)", "", "python",
          &["raise", "exception", "error", "ValueError"]),
        p("Match raise ValueError specifically", "raise ValueError($$$ARGS)", "", "python",
          &["raise", "ValueError", "error", "exception"]),
        p("Add line after a function call", "$VAR = $OBJ.$METHOD($$$ARGS)",
          "$VAR = $OBJ.$METHOD($$$ARGS)\nnew_statement_here", "python",
          &["add", "after", "insert", "line", "call"]),
        p("Add argument to function call", "re.compile($ARG)",
          "re.compile($ARG, re.IGNORECASE)", "python",
          &["argument", "add", "flag", "compile", "IGNORECASE", "case"]),
        p("Match .format() call on string", "$STR.format($$$ARGS)", "", "python",
          &["format", "string", "fstring"]),
        p("Replace method call", "$OBJ.old_method($$$ARGS)",
          "$OBJ.new_method($$$ARGS)", "python",
          &["method", "rename", "replace", "call"]),
        p("Add None check", "$OBJ.$METHOD($$$ARGS)",
          "if $OBJ is not None:\n    $OBJ.$METHOD($$$ARGS)", "python",
          &["None", "check", "guard", "null", "if"]),
        p("Match assignment", "$VAR = $VALUE", "", "python",
          &["assign", "variable", "set"]),
        p("Match return statement", "return $VALUE", "return $NEW", "python",
          &["return", "value", "change"]),
        p("Match if condition", "if $COND:\n    $$$BODY", "", "python",
          &["if", "condition", "branch"]),
        p("Match function definition", "def $NAME($$$PARAMS):\n    $$$BODY", "", "python",
          &["function", "def", "definition"]),
        p("Match class definition", "class $NAME($$$BASES):\n    $$$BODY", "", "python",
          &["class", "definition"]),
        p("Match import", "import $MODULE", "", "python",
          &["import", "module"]),
        p("Match from import", "from $MODULE import $$$NAMES", "", "python",
          &["from", "import", "module"]),
        p("Match decorator", "@$DECORATOR\ndef $NAME($$$PARAMS):\n    $$$BODY", "", "python",
          &["decorator", "annotation"]),
        p("Match try/except", "try:\n    $$$TRY_BODY\nexcept $EXCEPTION:\n    $$$EXCEPT_BODY", "", "python",
          &["try", "except", "catch", "error", "handle"]),
        p("Match with statement", "with $CONTEXT as $VAR:\n    $$$BODY", "", "python",
          &["with", "context", "manager"]),
        p("Match list comprehension", "[$EXPR for $VAR in $ITER]", "", "python",
          &["list", "comprehension", "loop"]),
        p("Match dict access", "$DICT[$KEY]", "$DICT.get($KEY)", "python",
          &["dict", "access", "get", "key"]),
        p("Match equality comparison", "$X == $Y", "", "python",
          &["equal", "compare", "comparison"]),
        p("Make comparison case-insensitive", "$X == \"$STR\"",
          "$X.lower() == \"$STR\".lower()", "python",
          &["case", "insensitive", "lower", "upper", "compare"]),
        p("Match copy/deepcopy", "copy.deepcopy($ARG)", "copy.deepcopy($ARG, memo)", "python",
          &["copy", "deepcopy", "clone"]),
        p("Match isinstance check", "isinstance($OBJ, $TYPE)", "", "python",
          &["isinstance", "type", "check"]),
        p("Match property getter", "@property\ndef $NAME(self):\n    $$$BODY", "", "python",
          &["property", "getter"]),

        // ---- JavaScript/TypeScript ----
        p("Match function call", "$FUNC($$$ARGS)", "", "javascript",
          &["call", "function", "invoke"]),
        p("Replace var with const", "var $NAME = $VALUE", "const $NAME = $VALUE", "javascript",
          &["var", "const", "let", "declare"]),
        p("Match arrow function", "($$$PARAMS) => $BODY", "", "javascript",
          &["arrow", "function", "lambda"]),
        p("Match async/await", "await $EXPR", "", "javascript",
          &["async", "await", "promise"]),
        p("Match console.log", "console.log($$$ARGS)", "", "javascript",
          &["console", "log", "debug", "print"]),

        // ---- Rust ----
        p("Replace unwrap with ?", "$EXPR.unwrap()", "$EXPR?", "rust",
          &["unwrap", "error", "result", "option"]),
        p("Match function definition", "fn $NAME($$$PARAMS) -> $RET {\n    $$$BODY\n}", "", "rust",
          &["function", "fn", "definition"]),
        p("Match impl block", "impl $TYPE {\n    $$$METHODS\n}", "", "rust",
          &["impl", "implementation", "method"]),
        p("Match match expression", "match $EXPR {\n    $$$ARMS\n}", "", "rust",
          &["match", "pattern", "arm"]),

        // ---- Go ----
        p("Match error check", "if err != nil {\n    $$$BODY\n}", "", "go",
          &["error", "nil", "check", "handle"]),
        p("Match function", "func $NAME($$$PARAMS) $RET {\n    $$$BODY\n}", "", "go",
          &["function", "func", "definition"]),

        // ---- Generic (any language) ----
        p("Match any function call with wildcards", "$FUNC($$$ARGS)", "", "any",
          &["call", "function", "invoke", "generic"]),
        p("Match any method call", "$OBJ.$METHOD($$$ARGS)", "", "any",
          &["method", "call", "object", "invoke"]),
        p("Match any assignment", "$VAR = $VALUE", "", "any",
          &["assign", "set", "variable"]),
    ]
}

fn p(desc: &str, pattern: &str, rewrite: &str, lang: &str, tags: &[&str]) -> AstGrepPattern {
    AstGrepPattern {
        description: desc.to_string(),
        pattern: pattern.to_string(),
        rewrite: rewrite.to_string(),
        language: lang.to_string(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_raise_valueerror() {
        let results = search("raise ValueError error message python", "python", 3);
        assert!(!results.is_empty());
        assert!(results[0].0.pattern.contains("raise"));
    }

    #[test]
    fn search_re_compile() {
        let results = search("add IGNORECASE flag to re.compile", "python", 3);
        assert!(!results.is_empty());
        assert!(results[0].0.pattern.contains("re.compile"));
    }

    #[test]
    fn search_rust_unwrap() {
        let results = search("replace unwrap with question mark", "rust", 3);
        assert!(!results.is_empty());
        assert!(results[0].0.pattern.contains("unwrap"));
    }

    #[test]
    fn language_boost() {
        let py = search("function call", "python", 1);
        let js = search("function call", "javascript", 1);
        // Both should find results but language-specific ones boosted
        assert!(!py.is_empty());
        assert!(!js.is_empty());
    }

    #[test]
    fn format_output_not_empty() {
        let results = search("raise ValueError", "python", 3);
        let formatted = format_results(&results);
        assert!(formatted.contains("pattern:"));
        assert!(formatted.contains("NEVER match string literal"));
    }

    #[test]
    fn builtin_has_enough_patterns() {
        let patterns = builtin_patterns();
        assert!(patterns.len() >= 30, "Expected 30+ patterns, got {}", patterns.len());
    }
}
