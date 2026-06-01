#![allow(dead_code)]
// `dead_code` silenced — the per-format handlers that call these
// constructors land in subsequent commits of PR B. The attribute is
// removed once each function has a caller.

// Caption builders.
//
// Every handler that emits a visual block (Image / Document) MUST also
// populate `HandlerOutput.content` with a caption that describes the
// payload. Two reasons:
//
//   1. `query/src/compact.rs::strip_images` flattens visual tool_results
//      before the compact API call to avoid paying the image-token cost.
//      When the flatten happens, the caption is what the model retains —
//      so it must be informative on its own.
//   2. Providers without `supports_vision = true` (the majority today
//      including DeepSeek) never see the block — the caption IS the
//      whole tool_result for them.

use std::path::Path;

use super::limits::human_bytes;

/// Caption for an image inlined as a `ContentBlock::Image`.
/// Mirrors the format documented in the synthesis: `[Image: <path>, <WxH>,
/// <mime>, <bytes_human>]`.
pub fn image(path: &Path, width: u32, height: u32, media_type: &str, bytes: u64) -> String {
    format!(
        "[Image: {}, {}x{}, {}, {}]",
        path.display(),
        width,
        height,
        media_type,
        human_bytes(bytes)
    )
}

/// Caption for a PDF inlined as a `ContentBlock::Document` and/or with
/// text extraction. `pages_used` describes the subset of pages actually
/// emitted (`"1-5"`, `"1-50 of 200"`, etc.).
pub fn pdf(path: &Path, total_pages: usize, pages_used: &str, bytes: u64) -> String {
    format!(
        "[PDF: {}, {} pages, {}{}]",
        path.display(),
        total_pages,
        human_bytes(bytes),
        if pages_used.is_empty() {
            String::new()
        } else {
            format!(", showing {}", pages_used)
        }
    )
}

/// Caption for an XLSX/XLSM workbook — lists sheet count + names so the
/// model can ask for a specific sheet on the next read.
pub fn xlsx(path: &Path, sheets: &[String], active_sheet: &str) -> String {
    format!(
        "[XLSX: {}, {} sheets ({}), showing \"{}\"]",
        path.display(),
        sheets.len(),
        sheets.join(", "),
        active_sheet
    )
}

/// Caption for a DOCX document.
pub fn docx(path: &Path, bytes: u64) -> String {
    format!("[DOCX: {}, {}]", path.display(), human_bytes(bytes))
}

/// Caption for a PPTX deck.
pub fn pptx(path: &Path, slides: usize, bytes: u64) -> String {
    format!(
        "[PPTX: {}, {} slides, {}]",
        path.display(),
        slides,
        human_bytes(bytes)
    )
}

/// Caption for an archive listing.
pub fn archive(path: &Path, kind: &str, member_count: usize, compressed: u64) -> String {
    format!(
        "[{}: {}, {} members, {} compressed]",
        kind,
        path.display(),
        member_count,
        human_bytes(compressed)
    )
}

/// Caption for a graceful stub (legacy Office, unsupported archive, etc.).
/// The `recipe` is a one-line Bash command the user can run to extract
/// the content out-of-band.
pub fn stub(path: &Path, kind: &str, recipe: &str) -> String {
    format!(
        "[{}: {}. Not parsed in this build. {}]",
        kind,
        path.display(),
        recipe
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn image_caption_format() {
        let c = image(&PathBuf::from("/x/y.png"), 800, 600, "image/png", 12_345);
        assert!(c.starts_with("[Image: /x/y.png, 800x600, image/png, "));
        assert!(c.ends_with("]"));
    }

    #[test]
    fn pdf_caption_with_pages_subset() {
        let c = pdf(&PathBuf::from("/x/y.pdf"), 200, "1-50 of 200", 4_500_000);
        assert!(c.contains("200 pages"));
        assert!(c.contains("4.3 MiB"));
        assert!(c.contains("showing 1-50 of 200"));
    }

    #[test]
    fn pdf_caption_without_pages_subset() {
        let c = pdf(&PathBuf::from("/x/y.pdf"), 5, "", 50_000);
        assert!(c.contains("5 pages"));
        assert!(!c.contains("showing"));
    }

    #[test]
    fn xlsx_caption_lists_sheets() {
        let c = xlsx(
            &PathBuf::from("/x/y.xlsx"),
            &["Sheet1".to_string(), "Q1".to_string(), "Q2".to_string()],
            "Sheet1",
        );
        assert!(c.contains("3 sheets"));
        assert!(c.contains("Sheet1, Q1, Q2"));
        assert!(c.contains("\"Sheet1\""));
    }

    #[test]
    fn stub_caption_includes_recipe() {
        let c = stub(
            &PathBuf::from("/x/y.xls"),
            "Legacy XLS",
            "Use: libreoffice --headless --convert-to xlsx y.xls",
        );
        assert!(c.contains("Legacy XLS"));
        assert!(c.contains("libreoffice"));
    }
}
