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

        // ── Dispatch ───────────────────────────────────────────────────
        let mut out = match kind {
            Kind::Text => text::read_text(&path, params.offset, params.limit).await,
            Kind::Csv | Kind::Tsv => {
                tabular::read_tabular(&path, kind, params.offset, params.limit).await
            }
            Kind::Json | Kind::Jsonl | Kind::Xml | Kind::Html | Kind::Markdown | Kind::Notebook => {
                structured::read_structured(&path, kind, params.offset, params.limit).await
            }
            Kind::Svg => text::read_text(&path, params.offset, params.limit).await,
            Kind::ImagePng
            | Kind::ImageJpeg
            | Kind::ImageGif
            | Kind::ImageWebp
            | Kind::ImageBmp
            | Kind::ImageIco => image::read_image(&path, kind).await,
            Kind::Pdf => pdf::read_pdf(&path, params.pages.as_deref()).await,
            Kind::Xlsx | Kind::Docx | Kind::Pptx => ooxml::read_ooxml(&path, kind).await,
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
            | Kind::Rar => archive::read_archive(&path, kind).await,
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
