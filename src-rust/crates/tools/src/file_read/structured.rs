// Structured text handlers: JSON, JSONL, XML, HTML, Markdown, Notebook.
//
// All fall back on the text handler for the bulk of the content, with
// per-format banners and (for JSON / JSONL / IPYNB) lightweight
// validation passes so the model knows when a payload is malformed.
//
// HTML uses a minimal in-tree strip (no third-party html5ever): we
// drop `<script>` and `<style>` blocks and remove every other tag.
// This is intentionally crude — accurate HTML → text is a separate
// problem we leave to the model when it asks for the raw HTML.

#![allow(dead_code)]

use std::path::Path;

use serde_json::Value;
use tokio::fs;

use super::detect::Kind;
use super::limits::MAX_TEXT_BYTES;
use super::output::HandlerOutput;

/// Dispatch for the structured-text family. The dispatcher in mod.rs
/// hands off based on `Kind`; this module owns the per-format banners
/// and validation.
pub async fn read_structured(
    path: &Path,
    kind: Kind,
    offset: Option<usize>,
    limit: Option<usize>,
) -> HandlerOutput {
    match kind {
        Kind::Json => read_json(path, offset, limit).await,
        Kind::Jsonl => read_jsonl(path, offset, limit).await,
        Kind::Xml => read_xml_or_markup(path, offset, limit, "XML").await,
        Kind::Html => read_html(path, offset, limit).await,
        Kind::Markdown => read_xml_or_markup(path, offset, limit, "Markdown").await,
        Kind::Notebook => read_notebook(path).await,
        _ => super::text::read_text(path, offset, limit).await,
    }
}

async fn read_json(path: &Path, offset: Option<usize>, limit: Option<usize>) -> HandlerOutput {
    let mut out = super::text::read_text(path, offset, limit).await;
    if !out.is_error {
        // Parse the whole document (bounded by MAX_TEXT_BYTES upstream)
        // to validate. Annotation only; the content stays the raw text.
        let bytes = fs::read(path).await.ok().unwrap_or_default();
        let bytes_capped: Vec<u8> = bytes
            .iter()
            .take(MAX_TEXT_BYTES as usize)
            .copied()
            .collect();
        let banner = match serde_json::from_slice::<Value>(&bytes_capped) {
            Ok(_) => format!("[JSON: {} — valid JSON document.]\n", path.display()),
            Err(e) => format!(
                "[JSON: {} — malformed: {}. Showing raw text below.]\n",
                path.display(),
                e
            ),
        };
        out.content = format!("{}{}", banner, out.content);
    }
    out
}

async fn read_jsonl(path: &Path, offset: Option<usize>, limit: Option<usize>) -> HandlerOutput {
    let mut out = super::text::read_text(path, offset, limit).await;
    if !out.is_error {
        let bytes = fs::read(path).await.ok().unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        let total = text.lines().count();
        let bad: Vec<usize> = text
            .lines()
            .enumerate()
            .filter_map(|(i, l)| {
                let trimmed = l.trim();
                if trimmed.is_empty() {
                    None
                } else if serde_json::from_str::<Value>(trimmed).is_err() {
                    Some(i + 1)
                } else {
                    None
                }
            })
            .take(20)
            .collect();
        let banner = if bad.is_empty() {
            format!(
                "[JSONL: {} — {} lines, all parse as JSON.]\n",
                path.display(),
                total
            )
        } else {
            format!(
                "[JSONL: {} — {} lines, {} malformed (first offenders: {:?}).]\n",
                path.display(),
                total,
                bad.len(),
                bad
            )
        };
        out.content = format!("{}{}", banner, out.content);
    }
    out
}

async fn read_xml_or_markup(
    path: &Path,
    offset: Option<usize>,
    limit: Option<usize>,
    label: &str,
) -> HandlerOutput {
    let mut out = super::text::read_text(path, offset, limit).await;
    if !out.is_error {
        let banner = format!(
            "[{}: {} — showing raw text below.]\n",
            label,
            path.display()
        );
        out.content = format!("{}{}", banner, out.content);
    }
    out
}

async fn read_html(path: &Path, offset: Option<usize>, limit: Option<usize>) -> HandlerOutput {
    let mut out = super::text::read_text(path, offset, limit).await;
    if !out.is_error {
        // Apply a crude script/style strip on the rendered content.
        let stripped = strip_html(&out.content);
        let banner = format!(
            "[HTML: {} — script/style stripped. Re-Read with offset/limit for the raw markup.]\n",
            path.display()
        );
        out.content = format!("{}{}", banner, stripped);
    }
    out
}

/// Lightweight HTML → text. Drops `<script>` and `<style>` blocks (with
/// their content) and removes every other tag. NOT a parser — adversarial
/// markup will leak through.
pub fn strip_html(s: &str) -> String {
    let stripped = drop_block(s, "<script", "</script>");
    let stripped = drop_block(&stripped, "<style", "</style>");
    // Drop remaining tags.
    let mut out = String::with_capacity(stripped.len());
    let mut in_tag = false;
    for c in stripped.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
            out.push(' ');
        } else if !in_tag {
            out.push(c);
        }
    }
    out
}

fn drop_block(s: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let lower = s.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(start) = lower[cursor..].find(open) {
        let abs_start = cursor + start;
        out.push_str(&s[cursor..abs_start]);
        let after = abs_start + open.len();
        if let Some(end_rel) = lower[after..].find(close) {
            cursor = after + end_rel + close.len();
        } else {
            cursor = s.len();
            break;
        }
    }
    out.push_str(&s[cursor..]);
    out
}

async fn read_notebook(path: &Path) -> HandlerOutput {
    let bytes = match fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            return HandlerOutput::error_text(format!(
                "[Notebook read failed for {}: {}]",
                path.display(),
                e
            ))
        }
    };
    let json: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            return HandlerOutput::error_text(format!(
                "[Notebook: {} not valid JSON: {}]",
                path.display(),
                e
            ))
        }
    };

    let mut out = format!("[Notebook: {} — cells listed below.]\n\n", path.display());

    let cells = json
        .get("cells")
        .and_then(Value::as_array)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);

    for (idx, cell) in cells.iter().enumerate() {
        let cell_type = cell.get("cell_type").and_then(Value::as_str).unwrap_or("?");
        let exec_count = cell
            .get("execution_count")
            .and_then(Value::as_i64)
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".to_string());
        let source = cell
            .get("source")
            .map(|s| match s {
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(""),
                Value::String(s) => s.clone(),
                _ => String::new(),
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "[Cell {} ({}), execution_count={}]\n{}\n\n",
            idx + 1,
            cell_type,
            exec_count,
            source
        ));
    }
    out.push_str("[Cell outputs (images, stream output) intentionally dropped.]\n");
    HandlerOutput::success_text(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[tokio::test]
    async fn json_valid_gets_banner() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("a.json");
        std::fs::File::create(&p)
            .unwrap()
            .write_all(b"{\"x\": 1}")
            .unwrap();
        let out = read_json(&p, None, None).await;
        assert!(!out.is_error);
        assert!(out.content.starts_with("[JSON"));
        assert!(out.content.contains("valid JSON"));
    }

    #[tokio::test]
    async fn json_malformed_gets_warning() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("bad.json");
        std::fs::File::create(&p)
            .unwrap()
            .write_all(b"{not json")
            .unwrap();
        let out = read_json(&p, None, None).await;
        assert!(!out.is_error);
        assert!(out.content.contains("malformed"));
    }

    #[tokio::test]
    async fn jsonl_flags_bad_lines() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("data.jsonl");
        std::fs::File::create(&p)
            .unwrap()
            .write_all(b"{\"a\":1}\n{nope\n{\"b\":2}\n")
            .unwrap();
        let out = read_jsonl(&p, None, None).await;
        assert!(out.content.contains("malformed"));
    }

    #[tokio::test]
    async fn html_strip_removes_script_style() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("page.html");
        std::fs::File::create(&p)
            .unwrap()
            .write_all(
                b"<html><head><style>body{color:red}</style></head><body><p>hi</p><script>alert(1)</script></body></html>",
            )
            .unwrap();
        let out = read_html(&p, None, None).await;
        assert!(!out.content.contains("alert("));
        assert!(!out.content.contains("color:red"));
        assert!(out.content.contains("hi"));
    }

    #[tokio::test]
    async fn notebook_lists_cells_drops_outputs() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("nb.ipynb");
        let nb = serde_json::json!({
            "cells": [
                {"cell_type": "code", "execution_count": 1, "source": ["print('hi')"], "outputs": [{"output_type": "stream"}]},
                {"cell_type": "markdown", "source": ["# header"]}
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        });
        std::fs::File::create(&p)
            .unwrap()
            .write_all(nb.to_string().as_bytes())
            .unwrap();
        let out = read_notebook(&p).await;
        assert!(!out.is_error);
        assert!(out.content.contains("Cell 1 (code)"));
        assert!(out.content.contains("print('hi')"));
        assert!(out.content.contains("Cell 2 (markdown)"));
        assert!(out.content.contains("intentionally dropped"));
    }
}
