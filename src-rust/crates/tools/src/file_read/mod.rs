// FileRead tool: read files with optional line range, image / PDF
// support, OOXML / ODF / archive extraction.
//
// PR B replaced the legacy single-file implementation with a
// per-format handler split. This module is the dispatcher: it
// validates the path, applies the pre-flight size cap, sniffs the
// file's Kind (magic bytes first, extension as fallback), and
// routes to the appropriate handler module.
//
// Every handler returns a `HandlerOutput`; `output::HandlerOutput::
// finalize` converts that into the final `ToolResult` with the
// blocks-vs-text dispatch invariant centralised in one place.

mod archive;
mod caption;
mod detect;
mod image;
mod legacy_office;
mod limits;
mod odf;
mod ooxml;
mod output;
mod pdf;
mod structured;
mod tabular;
mod text;

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::debug;

use detect::Kind;

pub struct FileReadTool;

#[derive(Debug, Deserialize)]
struct FileReadInput {
    file_path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    /// PDF page selector — `"1-5,7,9-10"` style. Ignored for non-PDF
    /// files. Resolves the dead-advice bug in the legacy description
    /// that promoted this parameter without declaring it in the
    /// schema.
    #[serde(default)]
    pages: Option<String>,
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        cc_core::constants::TOOL_NAME_FILE_READ
    }

    fn description(&self) -> &str {
        "Reads a file from the local filesystem and returns it in a format the model can \
         consume. Text files are returned line-numbered (default 2000 lines from the start). \
         Images (PNG / JPEG / GIF / WebP / BMP) are attached as Image blocks on \
         vision-capable providers, with a textual caption for the rest. PDFs are extracted \
         to text AND attached as Document blocks on vision providers; use `pages` to \
         request a sub-range. XLSX / DOCX / PPTX / ODT / ODS / ODP are extracted to text. \
         ZIP / TAR / TAR.GZ produce a textual manifest. Legacy XLS / DOC / PPT and \
         tar.bz2 / tar.xz / tar.zst / 7z / rar return a Bash recipe to convert them \
         out-of-band."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to read"
                },
                "offset": {
                    "type": "number",
                    "description": "Line number to start reading from (1-based). Only provide if the file is too large to read at once."
                },
                "limit": {
                    "type": "number",
                    "description": "Number of lines to read. Only provide if the file is too large to read at once."
                },
                "pages": {
                    "type": "string",
                    "description": "PDF page range selector like '1-5,7,9-10' (1-based). Ignored for non-PDF files."
                }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let params: FileReadInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let path = ctx.resolve_path(&params.file_path);
        debug!(path = %path.display(), "Reading file");

        // ── Existence + directory guard ────────────────────────────────
        if !path.exists() {
            return ToolResult::error(format!("File not found: {}", path.display()));
        }
        if path.is_dir() {
            return ToolResult::error(format!(
                "{} is a directory, not a file. Use Bash with `ls` to list directory contents.",
                path.display()
            ));
        }

        // ── Pre-flight size cap ────────────────────────────────────────
        //
        // The most important fix in PR B. The legacy path slurped the
        // whole file into memory with no guard, so a multi-GB file
        // would OOM the agent. Bound the size BEFORE the handler.
        match tokio::fs::metadata(&path).await {
            Ok(meta) if meta.len() > limits::MAX_FILE_BYTES => {
                return ToolResult::error(format!(
                    "[File too large: {} = {} > {} cap. \
                     Use Bash with head/tail/sed to read a slice, or split the file.]",
                    path.display(),
                    limits::human_bytes(meta.len()),
                    limits::human_bytes(limits::MAX_FILE_BYTES),
                ));
            }
            Ok(_) => {}
            Err(e) => {
                return ToolResult::error(format!(
                    "Failed to read file metadata for {}: {}",
                    path.display(),
                    e
                ));
            }
        }

        // ── Sniff: magic bytes first, extension fallback ───────────────
        //
        // Read up to 4 KiB to feed the sniffer. Empty files take the
        // text path (yields a "exists but is empty" message).
        let mut head = vec![0u8; 4096];
        let head_len = match tokio::fs::File::open(&path).await {
            Ok(mut f) => {
                use tokio::io::AsyncReadExt as _;
                f.read(&mut head).await.unwrap_or(0)
            }
            Err(_) => 0,
        };
        head.truncate(head_len);

        let raw_kind = detect::sniff(&path, &head);
        let kind = match raw_kind {
            Kind::Zip => detect::refine_zip(&path, raw_kind),
            Kind::TarGz => detect::refine_gzip(&path),
            other => other,
        };

        // ── Sniff vs. extension disagreement note ──────────────────────
        //
        // When magic bytes pick a kind that disagrees with the
        // extension, the dispatcher prepends a note to the textual
        // content so the model knows the file isn't what its name
        // suggested. Only fires for kinds that have a distinctive
        // ext (skip Text / Unknown).
        let ext_kind = ext_only_kind(&path);
        let disagreement = if disagrees(kind, ext_kind) {
            Some(format!(
                "[detected as {} via magic bytes; extension said {}]\n",
                kind.label(),
                ext_kind.label()
            ))
        } else {
            None
        };

        // Resolve runtime caps from Config (Batch 1: --max-text-bytes,
        // --max-line-chars, --default-read-line-limit, --max-image-bytes).
        let text_limits = text::TextLimits {
            max_text_bytes: ctx.config.effective_max_text_bytes(),
            max_line_chars: ctx.config.effective_max_line_chars(),
            default_line_limit: ctx.config.effective_default_read_line_limit(),
        };
        let max_image_bytes = ctx.config.effective_max_image_bytes();

        // ── Dispatch ───────────────────────────────────────────────────
        let mut out = match kind {
            Kind::Text => {
                text::read_text_with_limits(&path, params.offset, params.limit, text_limits).await
            }
            Kind::Csv | Kind::Tsv => {
                tabular::read_tabular(&path, kind, params.offset, params.limit).await
            }
            Kind::Json | Kind::Jsonl | Kind::Xml | Kind::Html | Kind::Markdown | Kind::Notebook => {
                structured::read_structured(&path, kind, params.offset, params.limit).await
            }
            Kind::Svg => {
                text::read_text_with_limits(&path, params.offset, params.limit, text_limits).await
            }
            Kind::ImagePng
            | Kind::ImageJpeg
            | Kind::ImageGif
            | Kind::ImageWebp
            | Kind::ImageBmp
            | Kind::ImageIco => image::read_image_with_limit(&path, kind, max_image_bytes).await,
            Kind::Pdf => pdf::read_pdf(&path, params.pages.as_deref(), &ctx.config).await,
            Kind::Xlsx | Kind::Docx | Kind::Pptx => {
                ooxml::read_ooxml(&path, kind, &ctx.config).await
            }
            Kind::Ods | Kind::Odt | Kind::Odp => odf::read_odf(&path, kind).await,
            Kind::LegacyXls | Kind::LegacyDoc | Kind::LegacyPpt => {
                legacy_office::read_legacy_office(&path, kind).await
            }
            Kind::Zip
            | Kind::TarPlain
            | Kind::TarGz
            | Kind::TarBz2
            | Kind::TarXz
            | Kind::TarZst
            | Kind::SevenZ
            | Kind::Rar => archive::read_archive(&path, kind, &ctx.config).await,
            Kind::Unknown => text::read_text(&path, params.offset, params.limit).await,
        };

        if let Some(note) = disagreement {
            out.content = format!("{}{}", note, out.content);
        }

        out.finalize(&path)
    }
}

/// Compute the Kind a file would be assigned if we IGNORED magic bytes.
/// Used for the "extension disagreement" note — comparing this against
/// the sniffed Kind tells us when the two are out of sync.
fn ext_only_kind(path: &std::path::Path) -> Kind {
    detect::sniff(path, &[])
}

/// Whether the magic-byte sniff disagrees with the extension. Fires
/// only when the sniff committed to a *specific* binary kind that the
/// extension doesn't predict — exactly the case where we want to warn
/// the model that the file isn't what its name suggested.
///
/// Suppressed when:
///   - sniffed == ext_only (they agree)
///   - sniffed is Text/Unknown (no commitment from magic)
///   - sniffed is Zip while ext was Xlsx/Docx/Pptx/Ods/Odt/Odp — the
///     refine_zip pass already promoted the kind, so they will agree
///     by the time we reach the dispatcher.
fn disagrees(sniffed: Kind, ext_only: Kind) -> bool {
    if sniffed == ext_only {
        return false;
    }
    if matches!(sniffed, Kind::Text | Kind::Unknown) {
        return false;
    }
    // Zip → OOXML/ODF refinement already handled upstream.
    if matches!(sniffed, Kind::Zip)
        && matches!(
            ext_only,
            Kind::Xlsx | Kind::Docx | Kind::Pptx | Kind::Ods | Kind::Odt | Kind::Odp | Kind::Zip
        )
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::config::Config;
    use cc_core::permissions::AutoPermissionHandler;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn test_ctx(working_dir: PathBuf) -> ToolContext {
        let handler = Arc::new(AutoPermissionHandler {
            mode: cc_core::config::PermissionMode::Default,
        });
        ToolContext {
            working_dir,
            permission_mode: cc_core::config::PermissionMode::Default,
            permission_handler: handler,
            cost_tracker: cc_core::cost::CostTracker::new(),
            session_id: "test".to_string(),
            file_history: Arc::new(parking_lot::Mutex::new(
                cc_core::file_history::FileHistory::new(),
            )),
            current_turn: Arc::new(AtomicUsize::new(0)),
            non_interactive: true,
            mcp_manager: None,
            config: Config::default(),
        }
    }

    #[tokio::test]
    async fn preflight_rejects_file_larger_than_cap() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("huge.txt");
        let file = std::fs::File::create(&path).expect("create");
        use std::io::{Seek, SeekFrom, Write};
        let mut f = file;
        f.seek(SeekFrom::Start(limits::MAX_FILE_BYTES + 1))
            .expect("seek");
        f.write_all(b"x").expect("write 1 byte at the end");
        drop(f);

        let ctx = test_ctx(tmp.path().to_path_buf());
        let tool = FileReadTool;
        let input = json!({ "file_path": path.to_str().unwrap() });
        let result = tool.execute(input, &ctx).await;
        assert!(result.is_error, "huge file must be refused");
        assert!(result.content.contains("File too large"));
    }

    #[tokio::test]
    async fn preflight_passes_small_text_file() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("small.txt");
        std::fs::write(&path, "line one\nline two\n").expect("write");

        let ctx = test_ctx(tmp.path().to_path_buf());
        let tool = FileReadTool;
        let input = json!({ "file_path": path.to_str().unwrap() });
        let result = tool.execute(input, &ctx).await;
        assert!(!result.is_error);
        assert!(result.content.contains("line one"));
        assert!(result.content.contains("line two"));
    }

    // ── Integration sweep ─────────────────────────────────────────────
    //
    // Each test exercises the full dispatcher on a different Kind with
    // a self-contained fixture. Together they assert the user's
    // verbatim French objective: "tt doit etre envoyé" — every format
    // the model can throw at FileRead must come back with usable
    // content (success), even when the body is a recipe stub.

    #[tokio::test]
    async fn integration_png_produces_image_block() {
        // 1×1 transparent PNG from image::tests.
        let tiny_png: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("tiny.png");
        std::fs::write(&path, tiny_png).unwrap();
        let ctx = test_ctx(tmp.path().to_path_buf());
        let result = FileReadTool
            .execute(json!({ "file_path": path.to_str().unwrap() }), &ctx)
            .await;
        assert!(!result.is_error);
        let blocks = result.blocks.expect("PNG must produce Image block");
        assert_eq!(blocks.len(), 1);
    }

    #[tokio::test]
    async fn integration_csv_carries_banner() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("data.csv");
        std::fs::write(&path, "a,b,c\n1,2,3\n").unwrap();
        let ctx = test_ctx(tmp.path().to_path_buf());
        let result = FileReadTool
            .execute(json!({ "file_path": path.to_str().unwrap() }), &ctx)
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("[CSV"));
    }

    #[tokio::test]
    async fn integration_json_validates() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("data.json");
        std::fs::write(&path, b"{\"k\":42}").unwrap();
        let ctx = test_ctx(tmp.path().to_path_buf());
        let result = FileReadTool
            .execute(json!({ "file_path": path.to_str().unwrap() }), &ctx)
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("valid JSON"));
    }

    #[tokio::test]
    async fn integration_zip_lists_members() {
        use std::io::Cursor;
        use std::io::Write;
        let mut buf = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut writer = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file("inside.txt", opts).unwrap();
            writer.write_all(b"hello").unwrap();
            writer.finish().unwrap();
        }
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.zip");
        std::fs::write(&path, &buf).unwrap();
        let ctx = test_ctx(tmp.path().to_path_buf());
        let result = FileReadTool
            .execute(json!({ "file_path": path.to_str().unwrap() }), &ctx)
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("inside.txt"));
    }

    #[tokio::test]
    async fn integration_legacy_xls_returns_recipe() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("legacy.xls");
        std::fs::write(&path, b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1placeholder").unwrap();
        let ctx = test_ctx(tmp.path().to_path_buf());
        let result = FileReadTool
            .execute(json!({ "file_path": path.to_str().unwrap() }), &ctx)
            .await;
        assert!(
            !result.is_error,
            "legacy stub is informational, not an error"
        );
        assert!(result.content.contains("libreoffice"));
        assert!(result.content.contains("xlsx"));
    }

    #[tokio::test]
    async fn integration_html_strips_script() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("page.html");
        std::fs::write(
            &path,
            b"<html><body><p>visible</p><script>secret()</script></body></html>",
        )
        .unwrap();
        let ctx = test_ctx(tmp.path().to_path_buf());
        let result = FileReadTool
            .execute(json!({ "file_path": path.to_str().unwrap() }), &ctx)
            .await;
        assert!(!result.is_error);
        assert!(!result.content.contains("secret("));
        assert!(result.content.contains("visible"));
    }

    #[tokio::test]
    async fn integration_notebook_lists_cells() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nb.ipynb");
        let nb = serde_json::json!({
            "cells": [
                {"cell_type": "code", "execution_count": 1, "source": ["print(1)"], "outputs": []},
            ],
            "metadata": {},
            "nbformat": 4, "nbformat_minor": 5
        });
        std::fs::write(&path, nb.to_string()).unwrap();
        let ctx = test_ctx(tmp.path().to_path_buf());
        let result = FileReadTool
            .execute(json!({ "file_path": path.to_str().unwrap() }), &ctx)
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("Cell 1 (code)"));
    }

    #[tokio::test]
    async fn integration_tar_xz_returns_recipe() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("foo.tar.xz");
        // Magic bytes for xz: FD 37 7A 58 5A 00
        std::fs::write(&path, b"\xfd7zXZ\x00placeholder").unwrap();
        let ctx = test_ctx(tmp.path().to_path_buf());
        let result = FileReadTool
            .execute(json!({ "file_path": path.to_str().unwrap() }), &ctx)
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("tar -xJf"));
    }

    #[tokio::test]
    async fn extension_disagreement_note_prepended() {
        // Write a PDF with a .txt extension — sniff should rule "PDF"
        // and the dispatcher should prepend the disagreement note.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("lying.txt");
        // Just the magic header is enough for the sniff. pdf-extract
        // will fail on the truncated body, which is fine: the handler
        // returns success_text with the caption + a "[text extraction
        // failed]" note.
        std::fs::write(&path, b"%PDF-1.4\n").expect("write");

        let ctx = test_ctx(tmp.path().to_path_buf());
        let tool = FileReadTool;
        let input = json!({ "file_path": path.to_str().unwrap() });
        let result = tool.execute(input, &ctx).await;
        assert!(!result.is_error);
        assert!(
            result.content.contains("detected as PDF via magic bytes"),
            "expected disagreement note, got: {}",
            result.content.chars().take(400).collect::<String>()
        );
    }
}
