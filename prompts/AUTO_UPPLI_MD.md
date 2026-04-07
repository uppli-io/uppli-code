# Auto-generate UPPLI.md on first launch

When uppli-code starts in a directory that has no UPPLI.md, it should:

1. Scan the project (ls, detect package.json/Cargo.toml/requirements.txt/go.mod)
2. Generate a UPPLI.md with: language, framework, structure, conventions
3. Save it to the project root
4. Include it in the system context for every subsequent call

This is what Claude Code does with CLAUDE.md. Without it, one-shot mode doesn't know the project tech stack and makes wrong decisions (e.g. creating Node.js files in a Python project).

Implementation: in main.rs or the context builder, before the first query, check if UPPLI.md exists. If not, use the LLM to generate one from a Glob + Read of key files (package.json, Cargo.toml, etc.), then save it.

This should be a one-time cost per project, not per session.
