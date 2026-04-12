# Compile uppli-code on bookworm WITHOUT cc-rag (no ort-sys / ONNX)
FROM rust:bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    libasound2-dev pkg-config cmake g++ python3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY src-rust/ ./src-rust/

WORKDIR /build/src-rust

# Strip cc-rag from the build entirely
RUN sed -i '/"rag"/d' Cargo.toml && \
    sed -i '/cc-rag/d' crates/cli/Cargo.toml crates/tools/Cargo.toml && \
    sed -i '/cc_rag/d' crates/cli/src/main.rs && \
    sed -i '/ast_grep_helper/d' crates/tools/src/lib.rs && \
    sed -i '/AstGrepHelperTool/d' crates/tools/src/lib.rs

# Remove RAG code from tool_search.rs using a heredoc script
COPY <<'PATCHSCRIPT' /tmp/patch_rag.py
import re
with open('crates/tools/src/tool_search.rs') as f:
    code = f.read()
code = re.sub(r'const RAG_TRIGGER_KEYWORDS.*?\n\}\n', '', code, flags=re.DOTALL)
code = re.sub(r'fn should_search_rag.*?\n\}\n', '', code, flags=re.DOTALL)
code = re.sub(r'        // RAG vector search.*?\n        \}\n', '', code, flags=re.DOTALL)
with open('crates/tools/src/tool_search.rs', 'w') as f:
    f.write(code)
PATCHSCRIPT
RUN python3 /tmp/patch_rag.py

RUN cargo build --release --bin uppli-code
RUN ls -la target/release/uppli-code
