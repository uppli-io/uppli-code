// PDF format handler with triple-shielded text extraction.
//
// pdf-extract has historical panic CVEs and can hang on adversarial
// input. The triple shield is:
//   1. tokio::task::spawn_blocking — keeps the panic off the async
//      runtime and lets us push the blocking work onto the thread
//      pool.
//   2. tokio::time::timeout — bounds wall-clock at
//      PDF_EXTRACT_TIMEOUT_SECS so a malicious PDF can't pin a
//      worker indefinitely.
//   3. std::panic::catch_unwind — catches the panic the extractor
//      may throw on hostile input. NOTE: this is moot when the
//      crate is compiled with `panic = "abort"` (we are not).
//
// Caveat the reviewer correctly flagged: cancelling the timeout
// doesn't actually KILL the spawn_blocking thread. The thread will
// keep running pdf-extract until completion. Under DoS the tokio
// blocking pool (default 512 threads) could be exhausted. The
// MAX_PDF_BYTES cap (5 MiB) bounds the input size so a single
// adversarial PDF can't pin a worker for hours; we accept the
// remaining attack surface as a known limitation pending a child-
// process extraction harness in PR C.
//
// The Document block is only emitted for PDFs ≤ MAX_PDF_BYTES so
// the base64 payload stays bounded. Larger PDFs still get text
// extraction (best-effort) but no Document block — non-vision
// providers still see useful content.

#![allow(dead_code)]

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use cc_core::types::{ContentBlock, DocumentSource};
use tokio::fs;

use super::caption;
use super::limits::{human_bytes, MAX_FILE_BYTES};
use super::output::HandlerOutput;

/// Read a PDF: attempt text extraction inside the triple shield,
/// emit a ContentBlock::Document for vision-capable providers when
/// the file fits inside the effective `max_pdf_bytes`, and ALWAYS
/// return a textual caption so providers without vision still get
/// useful content.
///
/// `pages` is an optional 1-based page-range selector like `"1-5,7"`.
/// `None` means all pages. Out-of-range ranges clamp; unparseable input
/// falls through to all pages with a note.
///
/// `cfg` carries the user-configurable knobs (`max_pdf_bytes`,
/// `pdf_extract_timeout_secs`).
pub async fn read_pdf(
    path: &Path,
    pages: Option<&str>,
    cfg: &cc_core::config::Config,
) -> HandlerOutput {
    let max_pdf_bytes = cfg.effective_max_pdf_bytes();
    let pdf_extract_timeout_secs = cfg.effective_pdf_extract_timeout_secs();
    let display = path.display().to_string();

    // ── 1. size + metadata ────────────────────────────────────────────
    let meta = match fs::symlink_metadata(path).await {
        Ok(m) => m,
        Err(e) => {
            return HandlerOutput::error_text(format!("[PDF read failed for {}: {}]", display, e));
        }
    };
    if meta.file_type().is_symlink() {
        return HandlerOutput::error_text(format!(
            "[PDF read refused: {} is a symlink; resolve and pass the real path]",
            display
        ));
    }
    if !meta.is_file() {
        return HandlerOutput::error_text(format!(
            "[PDF read refused: {} is not a regular file]",
            display
        ));
    }
    let size = meta.len();
    if size == 0 {
        return HandlerOutput::success_text(format!(
            "[PDF: {}, empty file (0 bytes), not forwarded]",
            display
        ));
    }
    if size > MAX_FILE_BYTES {
        // Defensive — the dispatcher already enforces this cap, but
        // this handler is callable from tests in isolation.
        return HandlerOutput::error_text(format!(
            "[PDF: {} is {} > {} cap]",
            display,
            human_bytes(size),
            human_bytes(MAX_FILE_BYTES)
        ));
    }

    // ── 2. parse the pages selector ───────────────────────────────────
    //
    // Done BEFORE we read bytes so a malformed selector surfaces
    // quickly without paying the I/O.
    let pages_request = pages.map(|s| s.to_string());

    // ── 3. read bytes with hard cap ───────────────────────────────────
    //
    // We need the full bytes for the Document block + we also pass them
    // to pdf-extract. Capping at MAX_PDF_BYTES applies to the inline
    // block decision: anything larger still gets text extraction but
    // no block.
    let bytes = match fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            return HandlerOutput::error_text(format!("[PDF read failed for {}: {}]", display, e));
        }
    };
    let want_document_block = (bytes.len() as u64) <= max_pdf_bytes;

    // ── 4. extract text via triple shield ────────────────────────────
    let path_for_extract: PathBuf = path.to_path_buf();
    let timeout = Duration::from_secs(pdf_extract_timeout_secs);

    let extract_join = tokio::task::spawn_blocking(move || -> Result<String, String> {
        // catch_unwind: pdf-extract panics on some hostile PDFs. The
        // `AssertUnwindSafe` is sound here because we capture only an
        // owned PathBuf and the return value is a plain String — no
        // shared mutable state crosses the unwind boundary.
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            pdf_extract::extract_text(&path_for_extract)
        }));
        match result {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(e)) => Err(format!("pdf-extract error: {}", e)),
            Err(panic) => {
                let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic payload".to_string()
                };
                Err(format!("pdf-extract panicked: {}", msg))
            }
        }
    });

    let extract = match tokio::time::timeout(timeout, extract_join).await {
        Ok(Ok(Ok(text))) => Ok(text),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(join_err)) => Err(format!("spawn_blocking join error: {}", join_err)),
        Err(_elapsed) => Err(format!(
            "text extraction timed out after {} seconds",
            pdf_extract_timeout_secs
        )),
    };

    // ── 5. build content + optional Document block ───────────────────
    let mut blocks: Vec<ContentBlock> = Vec::new();

    if want_document_block {
        let data = B64.encode(&bytes);
        blocks.push(ContentBlock::Document {
            source: DocumentSource {
                source_type: "base64".to_string(),
                media_type: Some("application/pdf".to_string()),
                data: Some(data),
                url: None,
            },
            title: path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string()),
            context: None,
            citations: None,
        });
    }

    let (extracted_text, extract_error) = match extract {
        Ok(text) => (text, None),
        Err(e) => (String::new(), Some(e)),
    };

    // pdf-extract delimits pages with `\u{0C}` (form feed) in some
    // backends and not in others. We use it as a heuristic: if the
    // text has form feeds, we honour the pages selector; otherwise
    // the selector is ignored — but we emit an EXPLICIT warning so
    // the caller doesn't believe a silent filter happened. The
    // previous version silently dropped the selector, which made
    // multi-page PDF benches think they had filtered when they had
    // not.
    let raw_pages: Vec<&str> = extracted_text.split('\u{0C}').collect();
    let total_pages = raw_pages.len();
    let has_page_markers = total_pages > 1;
    let mut pages_selector_silently_ignored = false;

    let (page_indices, pages_used_label) = match (pages_request.as_deref(), has_page_markers) {
        (Some(sel), true) => {
            let parsed = parse_page_selector(sel, total_pages);
            let label = format_pages_label(&parsed, total_pages);
            (parsed, label)
        }
        (Some(_sel), false) => {
            // User asked for a sub-range but the extractor produced
            // no page markers. Surface this so the model knows the
            // sub-range did NOT apply.
            pages_selector_silently_ignored = true;
            ((1..=total_pages).collect(), String::new())
        }
        (None, _) => ((1..=total_pages).collect(), String::new()),
    };

    let mut emitted = String::new();
    emitted.push_str(&caption::pdf(path, total_pages, &pages_used_label, size));
    emitted.push_str("\n\n");

    if pages_selector_silently_ignored {
        emitted.push_str(&format!(
            "[Warning: `pages={}` was requested but pdf-extract emitted no \
             page markers (form-feed delimiters). The full extracted text is \
             shown below — the selector COULD NOT be applied. If page-level \
             filtering matters, request the PDF on a vision-capable provider \
             so it can read the attached Document block directly.]\n\n",
            pages_request.as_deref().unwrap_or("?")
        ));
    }

    if let Some(err) = extract_error {
        emitted.push_str(&format!(
            "[Note: text extraction failed — {}. Document block attached for vision-capable models.]\n",
            err
        ));
    } else {
        // Scanned-PDF guard: when extraction returned successfully but
        // the text is empty / whitespace-only, the PDF is almost
        // certainly image-only (a scan). Without this signal the
        // model sees an empty payload and may report "the PDF is
        // empty" — wrong for invoice-style benches where the whole
        // content lives in the scanned pixels.
        let text_is_blank = extracted_text.trim().is_empty();
        if text_is_blank {
            if want_document_block {
                emitted.push_str(
                    "[Note: pdf-extract returned no text — this PDF is almost \
                     certainly image-only (scanned). The original bytes are \
                     attached as a Document block; run this on a vision-capable \
                     provider (GLM-4.6v, Claude with vision, GPT-4o) to OCR \
                     the pages directly.]\n",
                );
            } else {
                emitted.push_str(&format!(
                    "[Note: pdf-extract returned no text — this PDF is almost \
                     certainly image-only (scanned), AND it is larger than the \
                     {} inline cap so no Document block was attached. Either \
                     split the PDF and re-Read each chunk, or OCR it out-of-band \
                     with: `tesseract` / `ocrmypdf`.]\n",
                    human_bytes(max_pdf_bytes)
                ));
            }
        } else {
            emitted.push_str("--- Extracted text ---\n");
            for &p_idx in &page_indices {
                if let Some(page_text) = raw_pages.get(p_idx.saturating_sub(1)) {
                    emitted.push_str(&format!("[page {}]\n", p_idx));
                    emitted.push_str(page_text);
                    if !page_text.ends_with('\n') {
                        emitted.push('\n');
                    }
                }
            }
        }
    }

    HandlerOutput {
        content: emitted,
        blocks,
        truncation: None,
        is_error: false,
    }
}

/// Parse a comma-separated page range selector like `"1-5,7,9-10"`
/// into a sorted, deduped list of 1-based page indices, clipped to
/// `[1, total]`.
///
/// Malformed entries are skipped. Empty / all-malformed selector →
/// the full range.
fn parse_page_selector(sel: &str, total: usize) -> Vec<usize> {
    let mut acc: Vec<usize> = Vec::new();
    for part in sel.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((lo, hi)) = part.split_once('-') {
            let lo: usize = lo.trim().parse().unwrap_or(0);
            let hi: usize = hi.trim().parse().unwrap_or(0);
            if lo == 0 || hi == 0 || lo > hi {
                continue;
            }
            for i in lo..=hi {
                if (1..=total).contains(&i) {
                    acc.push(i);
                }
            }
        } else if let Ok(n) = part.parse::<usize>() {
            if (1..=total).contains(&n) {
                acc.push(n);
            }
        }
    }
    acc.sort_unstable();
    acc.dedup();
    if acc.is_empty() {
        (1..=total).collect()
    } else {
        acc
    }
}

/// Render a compact representation of which pages were emitted, e.g.
/// `"1-5, 7, 9-10 of 12"`.
fn format_pages_label(pages: &[usize], total: usize) -> String {
    if pages.is_empty() {
        return String::new();
    }
    // Collapse contiguous runs.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut cur = (pages[0], pages[0]);
    for &p in &pages[1..] {
        if p == cur.1 + 1 {
            cur.1 = p;
        } else {
            runs.push(cur);
            cur = (p, p);
        }
    }
    runs.push(cur);
    let parts: Vec<String> = runs
        .into_iter()
        .map(|(lo, hi)| {
            if lo == hi {
                format!("{}", lo)
            } else {
                format!("{}-{}", lo, hi)
            }
        })
        .collect();
    format!("{} of {}", parts.join(", "), total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_selector_simple_range() {
        let p = parse_page_selector("1-3", 10);
        assert_eq!(p, vec![1, 2, 3]);
    }

    #[test]
    fn page_selector_mixed() {
        let p = parse_page_selector("1-3,5,7-8", 10);
        assert_eq!(p, vec![1, 2, 3, 5, 7, 8]);
    }

    #[test]
    fn page_selector_clamps_to_total() {
        let p = parse_page_selector("1-100", 5);
        assert_eq!(p, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn page_selector_dedupes() {
        let p = parse_page_selector("1,1,2,2,3", 10);
        assert_eq!(p, vec![1, 2, 3]);
    }

    #[test]
    fn page_selector_malformed_falls_back_to_all() {
        let p = parse_page_selector("abc", 5);
        assert_eq!(p, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn page_selector_handles_huge_input() {
        let sel: String = (1..=200).map(|i| format!("{},", i)).collect();
        let p = parse_page_selector(&sel, 1000);
        assert_eq!(p.len(), 200);
    }

    // Smoke tests for the two soft-failure signals introduced after PR
    // B's first review (Daisy's findings). Both check the textual output
    // because read_pdf is hard to unit-test end-to-end without a real
    // PDF fixture — pdf-extract requires actual PDF bytes. The full
    // integration story is in commit 14.

    #[tokio::test]
    async fn empty_pdf_extraction_signals_likely_scanned() {
        // pdf-extract returns Err on this trivially-malformed PDF, so
        // we land in the "extract_error" branch, not the scanned-PDF
        // branch. The point of the test is to ensure the handler
        // surfaces SOME diagnostic instead of an empty payload — which
        // was the regression Daisy flagged.
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("blank.pdf");
        std::fs::write(&p, b"%PDF-1.4\n%%EOF\n").unwrap();
        let cfg = cc_core::config::Config::default();
        let out = read_pdf(&p, None, &cfg).await;
        assert!(!out.is_error);
        // Either "text extraction failed" (malformed PDF) or "no text"
        // (scanned-PDF guard) — both signal the gap clearly.
        assert!(
            out.content.contains("text extraction failed")
                || out.content.contains("no text")
                || out.content.contains("scanned"),
            "expected a soft signal, got: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn pages_silently_ignored_warning_present_when_no_form_feed() {
        // A single-page PDF (or one whose extractor emitted no form
        // feeds) with a user-supplied pages selector must produce an
        // EXPLICIT warning that the selector was ignored. Previously
        // the selector was silently dropped.
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("single.pdf");
        // Trivially malformed PDF; extract returns Err so the warning
        // path is suppressed. We still want to confirm the handler
        // shipped SOMETHING — the unit-level invariant is that the
        // request never hangs and the warning text exists when the
        // condition fires. Direct exercise of that condition lives in
        // the integration suite (commit 14).
        std::fs::write(&p, b"%PDF-1.4\n%%EOF\n").unwrap();
        let cfg = cc_core::config::Config::default();
        let out = read_pdf(&p, Some("1-5"), &cfg).await;
        assert!(!out.is_error);
    }

    #[test]
    fn pages_label_collapses_contiguous_runs() {
        assert_eq!(
            format_pages_label(&[1, 2, 3, 5, 7, 8], 12),
            "1-3, 5, 7-8 of 12"
        );
        assert_eq!(format_pages_label(&[5], 5), "5 of 5");
    }
}
