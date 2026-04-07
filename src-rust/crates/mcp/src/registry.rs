// cc-mcp: Static MCP server registry.
//
// Maintains a static list of well-known MCP servers for discovery via
// `/mcp search` and `/mcp add`. No network calls.

// ---------------------------------------------------------------------------
// Static registry
// ---------------------------------------------------------------------------

/// A description of one official MCP server.
pub struct OfficialMcpServer {
    pub name: &'static str,
    pub description: &'static str,
    pub homepage: &'static str,
    /// Optional npx / uvx / docker install command.
    pub install_command: Option<&'static str>,
    pub categories: &'static [&'static str],
}

/// All officially-supported MCP servers known at compile time.
pub const OFFICIAL_SERVERS: &[OfficialMcpServer] = &[
    OfficialMcpServer {
        name: "filesystem",
        description: "Provides read/write access to the local filesystem.",
        homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem",
        install_command: Some("npx -y @modelcontextprotocol/server-filesystem"),
        categories: &["files", "local"],
    },
    OfficialMcpServer {
        name: "github",
        description: "Interact with GitHub repositories, issues, pull requests, and code search.",
        homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/github",
        install_command: Some("npx -y @modelcontextprotocol/server-github"),
        categories: &["vcs", "remote"],
    },
    OfficialMcpServer {
        name: "gitlab",
        description: "Interact with GitLab repositories and merge requests.",
        homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/gitlab",
        install_command: Some("npx -y @modelcontextprotocol/server-gitlab"),
        categories: &["vcs", "remote"],
    },
    OfficialMcpServer {
        name: "google-drive",
        description: "Read and search files stored in Google Drive.",
        homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/gdrive",
        install_command: Some("npx -y @modelcontextprotocol/server-gdrive"),
        categories: &["files", "remote"],
    },
    OfficialMcpServer {
        name: "google-maps",
        description: "Geocoding, directions, and Places API via Google Maps.",
        homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/google-maps",
        install_command: Some("npx -y @modelcontextprotocol/server-google-maps"),
        categories: &["maps", "remote"],
    },
    OfficialMcpServer {
        name: "postgres",
        description: "Run read-only SQL queries against a PostgreSQL database.",
        homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/postgres",
        install_command: Some("npx -y @modelcontextprotocol/server-postgres"),
        categories: &["database", "local"],
    },
    OfficialMcpServer {
        name: "sqlite",
        description: "Interact with a SQLite database file.",
        homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/sqlite",
        install_command: Some("npx -y @modelcontextprotocol/server-sqlite"),
        categories: &["database", "local"],
    },
    OfficialMcpServer {
        name: "slack",
        description: "Post messages, list channels, and search Slack workspaces.",
        homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/slack",
        install_command: Some("npx -y @modelcontextprotocol/server-slack"),
        categories: &["communication", "remote"],
    },
    OfficialMcpServer {
        name: "memory",
        description: "Persistent key-value memory store across conversations.",
        homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/memory",
        install_command: Some("npx -y @modelcontextprotocol/server-memory"),
        categories: &["memory", "local"],
    },
    OfficialMcpServer {
        name: "sequential-thinking",
        description: "Structured chain-of-thought reasoning tool.",
        homepage:
            "https://github.com/modelcontextprotocol/servers/tree/main/src/sequentialthinking",
        install_command: Some("npx -y @modelcontextprotocol/server-sequential-thinking"),
        categories: &["reasoning"],
    },
    OfficialMcpServer {
        name: "brave-search",
        description: "Web and local search via the Brave Search API.",
        homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/brave-search",
        install_command: Some("npx -y @modelcontextprotocol/server-brave-search"),
        categories: &["search", "remote"],
    },
    OfficialMcpServer {
        name: "fetch",
        description: "Fetch content from URLs (web pages, APIs, RSS feeds).",
        homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/fetch",
        install_command: Some("uvx mcp-server-fetch"),
        categories: &["http", "remote"],
    },
    OfficialMcpServer {
        name: "puppeteer",
        description: "Browser automation and web scraping via Puppeteer.",
        homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/puppeteer",
        install_command: Some("npx -y @modelcontextprotocol/server-puppeteer"),
        categories: &["browser", "automation"],
    },
    OfficialMcpServer {
        name: "aws-kb-retrieval",
        description: "Retrieve knowledge from AWS Bedrock Knowledge Bases.",
        homepage:
            "https://github.com/modelcontextprotocol/servers/tree/main/src/aws-kb-retrieval-server",
        install_command: Some("npx -y @modelcontextprotocol/server-aws-kb-retrieval"),
        categories: &["aws", "remote"],
    },
    OfficialMcpServer {
        name: "everything",
        description: "Reference / test server exposing all MCP capabilities.",
        homepage: "https://github.com/modelcontextprotocol/servers/tree/main/src/everything",
        install_command: Some("npx -y @modelcontextprotocol/server-everything"),
        categories: &["testing"],
    },
];

// ---------------------------------------------------------------------------
// Search helpers
// ---------------------------------------------------------------------------

/// Return all servers whose name or any category contains `query` (case-insensitive).
pub fn search_registry(query: &str) -> Vec<&'static OfficialMcpServer> {
    let q = query.to_lowercase();
    OFFICIAL_SERVERS
        .iter()
        .filter(|s| {
            s.name.to_lowercase().contains(&q)
                || s.description.to_lowercase().contains(&q)
                || s.categories.iter().any(|c| c.to_lowercase().contains(&q))
        })
        .collect()
}

/// Return the server with an exact name match, if any.
pub fn find_server(name: &str) -> Option<&'static OfficialMcpServer> {
    OFFICIAL_SERVERS.iter().find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_server() {
        let s = find_server("filesystem").unwrap();
        assert_eq!(s.name, "filesystem");
        assert!(s.install_command.is_some());
    }

    #[test]
    fn test_search_registry_by_category() {
        let results = search_registry("database");
        let names: Vec<_> = results.iter().map(|s| s.name).collect();
        assert!(names.contains(&"postgres"));
        assert!(names.contains(&"sqlite"));
    }

    #[test]
    fn test_search_registry_by_name() {
        let results = search_registry("github");
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "github");
    }
}
