// Tool expertise database — rich knowledge about each tool for intelligent
// selection and contextual guidance.
//
// Two use cases:
//   1. "I need to modify a Python file" → returns ranked tools with strengths/weaknesses
//   2. "I'm about to use Edit" → returns tips and best practices for that tool
//
// This replaces the thin keyword catalog in ToolSearch with a knowledge base
// that helps the model make informed tool choices.

/// Rich expertise entry for a single tool.
#[derive(Debug, Clone)]
pub struct ToolExpertise {
    /// Tool name (must match the Tool::name() return value).
    pub name: &'static str,
    /// One-line summary for system prompt listing.
    pub brief: &'static str,
    /// Detailed description returned when the model queries this tool.
    pub detail: &'static str,
    /// When to use this tool (positive signals).
    pub when_to_use: &'static [&'static str],
    /// When NOT to use this tool (negative signals / better alternatives).
    pub when_not_to_use: &'static [&'static str],
    /// Tips injected before tool execution.
    pub tips: &'static [&'static str],
    /// Tips injected when the tool returns an error.
    pub on_error_tips: &'static [&'static str],
    /// Alternative tools for the same task.
    pub alternatives: &'static [&'static str],
    /// Capability tags for semantic matching (lowercase).
    pub tags: &'static [&'static str],
}

/// Full expertise database — one entry per tool.
pub static EXPERTISE: &[ToolExpertise] = &[
    // ---- File operations ----
    ToolExpertise {
        name: "Read",
        brief: "Read file contents (shows line numbers)",
        detail: "Reads a file and returns its contents with line numbers (line_number\\tcontent format). \
                 Supports offset/limit for large files, PDF pages, images, and Jupyter notebooks.",
        when_to_use: &[
            "Before editing any file — always read first",
            "To understand code structure before making changes",
            "To read specific sections of large files with offset/limit",
        ],
        when_not_to_use: &[
            "To search for content across many files — use Grep instead",
            "To find files by name — use Glob instead",
        ],
        tips: &[
            "Output format is 'line_number\\tcontent'. Line numbers are NOT part of the file.",
            "For large files, use offset and limit to read specific sections.",
        ],
        on_error_tips: &[
            "File not found? Check the path with Glob.",
            "Permission denied? Check file permissions with Bash: ls -la",
        ],
        alternatives: &["Bash: cat, head, tail"],
        tags: &["file", "read", "view", "content", "inspect", "code"],
    },
    ToolExpertise {
        name: "Edit",
        brief: "Exact string replacement in files (syntax-checked)",
        detail: "Replaces an exact string in a file with a new string. The old_string must match \
                 the file content precisely, including all whitespace and indentation. A syntax \
                 check runs after each edit — if it introduces errors, the edit is reverted. \
                 Set replace_all=true to replace every occurrence.",
        when_to_use: &[
            "Small, targeted changes (1-10 lines)",
            "When you can precisely identify the text to replace",
            "When the change is in a single location",
        ],
        when_not_to_use: &[
            "Structural code changes with indentation — use AstEdit instead",
            "When the exact text is hard to specify — use AstEdit or Bash with sed",
        ],
        tips: &[
            "CRITICAL: Include at least 3 lines of context BEFORE and AFTER the target text.",
            "Match whitespace and indentation exactly as shown in Read output.",
            "Do NOT include line numbers from Read output — only the actual content after the tab.",
        ],
        on_error_tips: &[
            "old_string not found? Likely indentation mismatch. Try AstEdit for structural changes.",
            "Alternative: Bash with 'sed -i' for regex replacement.",
        ],
        alternatives: &["AstEdit", "Bash: sed -i", "Write (full file replacement)"],
        tags: &["file", "edit", "modify", "replace", "code", "string", "refactor"],
    },
    ToolExpertise {
        name: "Write",
        brief: "Write full file content (syntax-checked)",
        detail: "Writes content to a file, creating it if it doesn't exist. Overwrites existing \
                 content entirely. A syntax check runs after — if it fails, the write is reverted. \
                 Use for new files or complete rewrites.",
        when_to_use: &[
            "Creating new files",
            "Complete file rewrites where Edit would require too many replacements",
        ],
        when_not_to_use: &[
            "Small changes to existing files — use Edit instead",
            "Modifying part of a file — use Edit or AstEdit",
        ],
        tips: &[
            "Always read an existing file before overwriting to avoid losing content.",
        ],
        on_error_tips: &[],
        alternatives: &["Edit", "Bash: cat > file"],
        tags: &["file", "write", "create", "save", "new"],
    },
    ToolExpertise {
        name: "AstEdit",
        brief: "Structural code search and rewrite via AST (automatic indentation)",
        detail: "Uses ast-grep to find code by structural pattern and rewrite it. \
                 Operates on the Abstract Syntax Tree, not raw text — indentation is \
                 handled automatically. Use $VAR for single nodes, $$$VAR for multiple.",
        when_to_use: &[
            "Code changes where indentation matters (Edit fails on whitespace)",
            "Structural transformations (rename a pattern, add a line after a call)",
            "When you know the code pattern but not the exact text",
        ],
        when_not_to_use: &[
            "Simple text replacement (a value, a string) — Edit is faster",
            "Non-code files (markdown, yaml, json) — use Edit",
        ],
        tips: &[
            "$VAR matches one AST node, $$$VAR matches multiple nodes.",
            "Pattern must be valid code in the target language.",
            "Indentation in rewrite is automatic — don't add manual spaces.",
        ],
        on_error_tips: &[
            "Pattern not found? Check the language parameter and pattern syntax.",
            "Requires ast-grep (sg) installed: brew install ast-grep.",
        ],
        alternatives: &["Edit", "Bash: sed -i"],
        tags: &["file", "edit", "ast", "structural", "code", "rewrite", "indentation", "refactor"],
    },
    // ---- Search ----
    ToolExpertise {
        name: "Glob",
        brief: "Find files by name pattern",
        detail: "Fast file search using glob patterns (e.g., **/*.py, src/**/*.rs). Returns \
                 matching file paths sorted by modification time.",
        when_to_use: &[
            "Finding files by name or extension",
            "Exploring project structure",
            "Locating config files, tests, specific file types",
        ],
        when_not_to_use: &[
            "Searching file CONTENTS — use Grep instead",
        ],
        tips: &[
            "Use ** for recursive matching: **/*.py finds all Python files.",
        ],
        on_error_tips: &[],
        alternatives: &["Bash: find, ls"],
        tags: &["search", "find", "files", "pattern", "glob", "directory", "explore"],
    },
    ToolExpertise {
        name: "Grep",
        brief: "Search file contents by regex",
        detail: "Searches file contents using regex patterns (powered by ripgrep). Supports \
                 context lines (-A/-B/-C), file type filtering, and glob filtering. \
                 Three output modes: content (matching lines), files_with_matches, count.",
        when_to_use: &[
            "Finding where a function/class/variable is defined or used",
            "Searching for specific strings or patterns across the codebase",
            "Understanding code dependencies and call sites",
        ],
        when_not_to_use: &[
            "Finding files by name — use Glob instead",
            "Reading a whole file — use Read instead",
        ],
        tips: &[
            "Use context lines (-C 5) to see surrounding code.",
            "Use type filter (type: 'py') to limit to specific languages.",
            "Use output_mode: 'files_with_matches' for just file paths.",
        ],
        on_error_tips: &[],
        alternatives: &["Bash: grep, rg, ag"],
        tags: &["search", "find", "content", "regex", "code", "pattern", "grep"],
    },
    // ---- Execution ----
    ToolExpertise {
        name: "Bash",
        brief: "Run any shell command (no restrictions)",
        detail: "Executes shell commands with full access to the system. You can run any program: \
                 git, python, pip, npm, sed, awk, curl, docker, make, cargo, etc. \
                 You can install tools if needed (pip install, npm install, apt-get).",
        when_to_use: &[
            "Running tests, builds, linters",
            "Complex file operations that Edit can't handle (sed regex, awk)",
            "Installing dependencies or tools",
            "Git operations (commit, diff, log, apply)",
            "Any task that needs a shell command",
        ],
        when_not_to_use: &[
            "Simple file reads — Read is faster and shows line numbers",
            "Simple file searches — Grep is more structured",
        ],
        tips: &[
            "You can install any tool: pip install, npm install, brew install, apt-get install.",
            "For file editing: on macOS use sed -i '' 's/old/new/g' file (empty string after -i). On Linux use sed -i 's/old/new/g' file.",
            "Always quote file paths with spaces.",
            "For multi-line sed, use python -c or awk instead.",
        ],
        on_error_tips: &[
            "Command not found? Install it: pip install, npm install -g, brew install.",
            "Permission denied? Check with ls -la. Use chmod if needed.",
            "sed error on macOS? Use sed -i '' (with empty quotes after -i).",
        ],
        alternatives: &[],
        tags: &["shell", "command", "execute", "run", "terminal", "install", "test", "build",
                "git", "sed", "awk", "python", "npm", "pip"],
    },
    // ---- Web ----
    ToolExpertise {
        name: "WebFetch",
        brief: "Fetch and analyze web page content",
        detail: "Fetches a URL, converts HTML to markdown, and processes the content. \
                 Useful for reading documentation, API references, and web resources.",
        when_to_use: &["Reading online documentation", "Fetching API references", "Downloading content"],
        when_not_to_use: &["Searching the web — use WebSearch first to find URLs"],
        tips: &[],
        on_error_tips: &["URL requires auth? Use Bash with curl and proper headers."],
        alternatives: &["Bash: curl, wget"],
        tags: &["web", "fetch", "http", "url", "docs", "api"],
    },
    ToolExpertise {
        name: "WebSearch",
        brief: "Search the web",
        detail: "Performs a web search and returns results with titles, URLs, and snippets.",
        when_to_use: &["Finding documentation", "Researching APIs", "Looking up error messages"],
        when_not_to_use: &["If you already have the URL — use WebFetch directly"],
        tips: &[],
        on_error_tips: &[],
        alternatives: &["Bash: curl with search API"],
        tags: &["web", "search", "internet", "docs", "research"],
    },
    // ---- Planning & Tasks ----
    ToolExpertise {
        name: "Agent",
        brief: "Spawn a sub-agent for complex parallel tasks",
        detail: "Creates a new agent with its own tool context to handle a sub-task. \
                 The sub-agent runs the full agentic loop and returns the result.",
        when_to_use: &[
            "Delegating complex sub-tasks",
            "Parallelizing independent work",
            "Isolating risky operations",
        ],
        when_not_to_use: &["Simple tasks you can do directly"],
        tips: &["Give clear, self-contained instructions — the sub-agent has no context from your conversation."],
        on_error_tips: &[],
        alternatives: &[],
        tags: &["agent", "parallel", "delegate", "subtask", "complex"],
    },
    ToolExpertise {
        name: "TodoWrite",
        brief: "Track task progress",
        detail: "Create and manage a structured task list for tracking progress on multi-step work.",
        when_to_use: &["Multi-step tasks", "Showing progress to the user"],
        when_not_to_use: &["Single simple tasks"],
        tips: &[],
        on_error_tips: &[],
        alternatives: &[],
        tags: &["todo", "task", "progress", "plan", "track"],
    },
    ToolExpertise {
        name: "EnterPlanMode",
        brief: "Switch to planning mode (no tool execution)",
        detail: "Enters a mode where you plan the approach without executing tools. Useful for \
                 thinking through complex tasks before acting.",
        when_to_use: &["Complex tasks needing a plan first", "When user asks for a plan"],
        when_not_to_use: &["Simple tasks — just do them"],
        tips: &[],
        on_error_tips: &[],
        alternatives: &[],
        tags: &["plan", "think", "strategy", "design"],
    },
    ToolExpertise {
        name: "NotebookEdit",
        brief: "Edit Jupyter notebook cells",
        detail: "Edit, insert, or delete cells in Jupyter .ipynb notebooks.",
        when_to_use: &["Modifying Jupyter notebooks"],
        when_not_to_use: &["Regular files — use Edit or Write"],
        tips: &[],
        on_error_tips: &[],
        alternatives: &[],
        tags: &["notebook", "jupyter", "ipynb", "cell", "python", "data"],
    },
    ToolExpertise {
        name: "AskUserQuestion",
        brief: "Ask the user a question",
        detail: "Prompt the user for input when you need clarification or a decision.",
        when_to_use: &["Ambiguous requirements", "Need user confirmation for destructive actions"],
        when_not_to_use: &["When you can figure it out yourself"],
        tips: &[],
        on_error_tips: &[],
        alternatives: &[],
        tags: &["ask", "user", "question", "clarify", "input"],
    },
    ToolExpertise {
        name: "Skill",
        brief: "Execute a skill/slash command",
        detail: "Run a predefined skill (slash command) like /commit, /review, /test.",
        when_to_use: &["Executing standard workflows"],
        when_not_to_use: &[],
        tips: &[],
        on_error_tips: &[],
        alternatives: &[],
        tags: &["skill", "command", "slash", "workflow", "template"],
    },
    ToolExpertise {
        name: "LSP",
        brief: "Query language server for diagnostics, completions, definitions",
        detail: "Interface with running LSP servers for code intelligence: diagnostics, \
                 go-to-definition, find-references, completions.",
        when_to_use: &["Finding type errors", "Navigating to definitions", "Understanding code structure"],
        when_not_to_use: &["Simple text search — use Grep"],
        tips: &[],
        on_error_tips: &["LSP server not running? It starts automatically for detected languages."],
        alternatives: &["Grep (for text-based search)"],
        tags: &["lsp", "language", "server", "diagnostics", "types", "definition", "completion"],
    },
    ToolExpertise {
        name: "ToolSearch",
        brief: "Find the right tool for a task",
        detail: "Search for available tools by capability or name. Use when you're unsure \
                 which tool is best for a task — describe what you need to do and get \
                 ranked recommendations with strengths and tips.",
        when_to_use: &[
            "Unsure which tool to use for a task",
            "Want to compare alternatives before choosing",
            "Looking for a specific tool by name",
        ],
        when_not_to_use: &["When you already know which tool to use"],
        tips: &[
            "Describe the TASK, not the tool name: 'modify python file' instead of 'edit'.",
            "Use 'select:ToolName' for direct lookup with full details.",
        ],
        on_error_tips: &[],
        alternatives: &[],
        tags: &["search", "tool", "find", "help", "recommend", "which"],
    },
    // ---- Code analysis ----
    ToolExpertise {
        name: "CodeAudit",
        brief: "Audit a file for structural anomalies before fixing bugs",
        detail: "Runs 5 static analyzers (AST patterns, data flow, control flow, \
                 symbol table, consistency) on a source file. Returns a report of \
                 ALL anomalies found — inconsistent patterns, lossy error messages, \
                 unnormalized variables, etc. Call this BEFORE editing to ensure \
                 your fix is complete.",
        when_to_use: &[
            "BEFORE fixing any bug — always audit the target file first",
            "When a bug report mentions behavior that spans multiple code paths",
            "When you suspect the reported symptom might have related issues elsewhere in the file",
        ],
        when_not_to_use: &[
            "After fixing — use lint/LSP for post-edit validation",
            "For files you are only reading, not modifying",
        ],
        tips: &[
            "Pass focus_symbols extracted from the bug report for targeted analysis.",
            "Cross-reference the audit report with the bug description to find ALL violations.",
            "Fix ALL anomalies related to the bug, not just the reported one.",
        ],
        on_error_tips: &[
            "Requires Python 3 installed.",
            "The script is at scripts/code_audit/code_audit.py.",
        ],
        alternatives: &["Grep (for manual symbol search)", "LSP (for type errors only)"],
        tags: &["audit", "bug", "fix", "analyze", "static", "consistency",
                 "invariant", "verify", "check", "review", "pattern"],
    },
];

// ---------------------------------------------------------------------------
// Lookup functions
// ---------------------------------------------------------------------------

/// Get expertise for a specific tool by name (case-insensitive).
pub fn get(tool_name: &str) -> Option<&'static ToolExpertise> {
    EXPERTISE
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(tool_name))
}

/// Search expertise by capability tags and keywords.
/// Returns entries sorted by relevance score (highest first).
pub fn search(query: &str, max_results: usize) -> Vec<(&'static ToolExpertise, usize)> {
    let terms: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();

    if terms.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(&ToolExpertise, usize)> = EXPERTISE
        .iter()
        .filter_map(|entry| {
            let mut score = 0usize;
            let name_lower = entry.name.to_lowercase();

            for term in &terms {
                // Name match (highest weight)
                if name_lower == *term {
                    score += 20;
                } else if name_lower.contains(term.as_str()) {
                    score += 10;
                }

                // Tag match (high weight — semantic)
                for &tag in entry.tags {
                    if tag == term.as_str() {
                        score += 12;
                    } else if tag.contains(term.as_str()) || term.contains(tag) {
                        score += 4;
                    }
                }

                // Brief/detail match (medium weight)
                let brief_lower = entry.brief.to_lowercase();
                if brief_lower.contains(term.as_str()) {
                    score += 6;
                }

                // when_to_use match (high weight — intent matching)
                for &use_case in entry.when_to_use {
                    if use_case.to_lowercase().contains(term.as_str()) {
                        score += 8;
                    }
                }
            }

            if score > 0 {
                Some((entry, score))
            } else {
                None
            }
        })
        .collect();

    scored.sort_by_key(|s| std::cmp::Reverse(s.1));
    scored.truncate(max_results);
    scored
}

/// Format a tool expertise entry for display to the model.
pub fn format_full(entry: &ToolExpertise) -> String {
    let mut out = format!("## {}\n{}\n", entry.name, entry.detail);

    if !entry.when_to_use.is_empty() {
        out.push_str("\nWhen to use:\n");
        for s in entry.when_to_use {
            out.push_str(&format!("  - {}\n", s));
        }
    }
    if !entry.when_not_to_use.is_empty() {
        out.push_str("\nWhen NOT to use:\n");
        for s in entry.when_not_to_use {
            out.push_str(&format!("  - {}\n", s));
        }
    }
    if !entry.tips.is_empty() {
        out.push_str("\nTips:\n");
        for s in entry.tips {
            out.push_str(&format!("  - {}\n", s));
        }
    }
    if !entry.alternatives.is_empty() {
        out.push_str(&format!(
            "\nAlternatives: {}\n",
            entry.alternatives.join(", ")
        ));
    }
    out
}

/// Format search results for display to the model.
pub fn format_search_results(results: &[(&ToolExpertise, usize)]) -> String {
    if results.is_empty() {
        return "No matching tools found. Try different keywords.".to_string();
    }
    let mut out = String::new();
    for (entry, score) in results {
        out.push_str(&format!(
            "\n**{}** (relevance: {}) — {}\n",
            entry.name, score, entry.brief
        ));
        if !entry.when_to_use.is_empty() {
            out.push_str(&format!("  Best for: {}\n", entry.when_to_use[0]));
        }
        if !entry.alternatives.is_empty() {
            out.push_str(&format!(
                "  Alternatives: {}\n",
                entry.alternatives.join(", ")
            ));
        }
    }
    out.push_str("\nUse ToolSearch(\"select:ToolName\") for full details on any tool.\n");
    out
}

/// Get tips to inject before tool execution.
pub fn pre_execution_tips(tool_name: &str) -> Option<String> {
    let entry = get(tool_name)?;
    if entry.tips.is_empty() {
        return None;
    }
    let tips: Vec<&str> = entry.tips.to_vec();
    Some(tips.join(" "))
}

/// Get tips to inject when a tool returns an error.
pub fn on_error_tips(tool_name: &str) -> Option<String> {
    let entry = get(tool_name)?;
    if entry.on_error_tips.is_empty() {
        return None;
    }
    let tips: Vec<&str> = entry.on_error_tips.to_vec();
    Some(tips.join(" "))
}

/// Generate a brief tool listing for the system prompt (~500 tokens).
pub fn brief_listing() -> String {
    let mut out = String::from("Available tools:\n");
    for entry in EXPERTISE {
        out.push_str(&format!("- **{}** — {}\n", entry.name, entry.brief));
    }
    out.push_str(
        "\nUse ToolSearch to find the best tool for your task or get detailed usage info.\n",
    );
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_by_name() {
        assert!(get("Edit").is_some());
        assert!(get("edit").is_some());
        assert!(get("EDIT").is_some());
        assert!(get("nonexistent").is_none());
    }

    #[test]
    fn search_by_capability() {
        let results = search("modify python file", 5);
        assert!(!results.is_empty());
        // Edit should rank high for "modify file"
        let names: Vec<&str> = results.iter().map(|(e, _)| e.name).collect();
        assert!(
            names.contains(&"Edit"),
            "Expected Edit in results: {:?}",
            names
        );
    }

    #[test]
    fn search_returns_alternatives() {
        let results = search("edit code indentation", 5);
        let names: Vec<&str> = results.iter().map(|(e, _)| e.name).collect();
        // Should return Edit, AstEdit, and possibly Bash
        assert!(
            names.len() >= 2,
            "Expected multiple alternatives: {:?}",
            names
        );
    }

    #[test]
    fn brief_listing_covers_core_tools() {
        let listing = brief_listing();
        assert!(listing.contains("Edit"));
        assert!(listing.contains("Read"));
        assert!(listing.contains("Bash"));
        assert!(listing.contains("Grep"));
        assert!(listing.contains("AstEdit"));
    }

    #[test]
    fn pre_execution_tips_exist_for_edit() {
        let tips = pre_execution_tips("Edit");
        assert!(tips.is_some());
        assert!(tips.unwrap().contains("3 lines"));
    }

    #[test]
    fn on_error_tips_exist_for_edit() {
        let tips = on_error_tips("Edit");
        assert!(tips.is_some());
        assert!(tips.unwrap().contains("sed"));
    }
}
