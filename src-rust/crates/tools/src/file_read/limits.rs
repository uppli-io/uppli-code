#![allow(dead_code)]
// `dead_code` is silenced for the module because the per-format handler
// commits that consume these constants land in subsequent commits of PR B.
// Each commit removes a `dead_code` warning as it wires a handler up; the
// attribute is removed once every constant has at least one caller.

// Hard limits for the FileRead tool — single source of truth.
//
// Every per-format handler MUST respect these caps. They are intentionally
// conservative because the budget guard at `query/src/lib.rs::total_tool_result_chars`
// only counts text bytes, not the base64 payload of Image / Document blocks
// (TODO(pr-c): teach the budget guard to sum block payloads). Until that
// blind spot is fixed, the per-format byte caps below are the only thing
// keeping a single tool_result from injecting a multi-MiB base64 blob.

// ── Pre-flight ──────────────────────────────────────────────────────────────

/// Hard ceiling for any file the tool will OPEN, regardless of format.
/// Checked at the top of `execute` via `fs::metadata().len()` before any
/// branch reads bytes. A multi-GB log file MUST fail fast here instead of
/// being slurped whole into memory (the legacy behaviour).
///
/// Hardcoded: runaway-OOM safety guard. Fails loudly with a clear error
/// rather than silently truncating, so the user is never misled. 100 MiB
/// is a sane ceiling against accidentally mmap'ing a multi-GiB blob.
pub const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024; // 100 MiB

// ── Text ────────────────────────────────────────────────────────────────────

/// Cap on the bytes a text-path read will materialise as String.
/// Streamed via `BufReader.take(MAX_TEXT_BYTES)` — never load more than
/// this even for files that pass the pre-flight cap.
///
/// Re-exported from `cc_core::constants` so the value is a single source of
/// truth and stays configurable via `Config::max_text_bytes` / the
/// `--max-text-bytes` CLI flag.
pub use cc_core::constants::MAX_TEXT_BYTES;

/// Per-line truncation. A 1 MiB minified JS line would otherwise blow the
/// budget on a single line. Re-exported from `cc_core::constants` so the
/// runtime helper `Config::effective_max_line_chars()` shares the same
/// default.
pub use cc_core::constants::MAX_LINE_CHARS;

/// Default line count when the caller omits `limit` — preserves legacy
/// behaviour from the pre-PR-B implementation. Re-exported from
/// `cc_core::constants`.
pub use cc_core::constants::DEFAULT_LINE_LIMIT;

// ── Image ───────────────────────────────────────────────────────────────────

/// Hard cap on the raw image bytes the handler will inline as base64.
/// Beyond this → caption-only fallback. Re-exported from
/// `cc_core::constants`.
pub use cc_core::constants::MAX_IMAGE_BYTES;

/// Pixel-count cap probed BEFORE pixel decode (via `image::ImageReader::
/// with_guessed_format`). Defeats decompression bombs (a 10 KiB PNG that
/// decodes to 100k×100k pixels).
///
/// Hardcoded: decompression-bomb guard. A 100 KB malicious PNG can expand
/// to gigabytes in RAM — must remain a hard ceiling, not user-tunable.
pub const MAX_IMAGE_PIXELS: u64 = 8_000_000; // 8 MP

/// When an image exceeds MAX_IMAGE_PIXELS, downscale to this long-edge
/// in JPEG q=80 (mirrors the screenshot path in `computer_use.rs`).
pub const IMAGE_RESIZE_LONG_EDGE: u32 = 2048;

// ── PDF ─────────────────────────────────────────────────────────────────────

/// Cap on the PDF bytes inlined as a `ContentBlock::Document` for vision
/// providers. Beyond this we still attempt text extraction but emit
/// `blocks=None`.
pub const MAX_PDF_BYTES: u64 = 5 * 1024 * 1024; // 5 MiB

/// Cap on the number of pages the handler will emit text for.
pub const MAX_PDF_PAGES: usize = 50;

/// Wall-clock budget for `pdf-extract::extract_text`. The crate has
/// historical panic CVEs and can also hang on adversarial input; we
/// wrap it in `spawn_blocking` + `tokio::time::timeout` + `catch_unwind`.
pub const PDF_EXTRACT_TIMEOUT_SECS: u64 = 30;

// ── OOXML / ODF ─────────────────────────────────────────────────────────────

/// Cap on the compressed archive size we will open as XLSX / DOCX / PPTX
/// or their ODF cousins. The XML inside is bounded by its own depth cap.
///
/// Hardcoded: pre-flight safety guard on Office docs that fails loudly.
/// 20 MiB rejects truly unreasonable docs while allowing all realistic
/// ones; user gets a clear error rather than silent loss.
pub const MAX_OOXML_BYTES: u64 = 20 * 1024 * 1024; // 20 MiB

/// Cap on the number of XLSX rows emitted in the textual fallback.
pub const MAX_OOXML_ROWS: usize = 500;

/// Maximum nesting depth for XML elements inside OOXML / ODF parts.
/// Defeats billion-laughs / XXE / deeply-nested entities.
///
/// Hardcoded: protocol-level safety invariant. Uncapped recursion is a
/// stack-smash and DoS vector — this is not user policy.
pub const MAX_XML_DEPTH: u32 = 128;

// ── Archive ─────────────────────────────────────────────────────────────────

/// Cap on the archive file size on disk (the compressed view).
///
/// Hardcoded: zip-bomb pre-flight guard. Refuses to open archives that
/// could decompress to TB; rejection is loud (error), not silent.
pub const MAX_ARCHIVE_COMPRESSED: u64 = 50 * 1024 * 1024; // 50 MiB

/// Cap on the sum of all decompressed member sizes — global guard so a
/// 1 MiB zip with one 250 MiB entry can't push us into OOM territory.
///
/// Hardcoded: second-line zip-bomb guard once unpacking starts. Caps
/// RAM/disk blast radius regardless of declared per-entry sizes.
pub const MAX_ARCHIVE_DECOMPRESSED: u64 = 250 * 1024 * 1024; // 250 MiB

/// Cap on the per-member and global compression ratio. A 1 KiB archive
/// entry that claims to decompress to 200 KiB is rejected as a likely
/// zip bomb.
///
/// Hardcoded: classic zip-bomb heuristic — pure safety, no legitimate
/// workflow needs >100x ratio.
pub const MAX_COMPRESSION_RATIO: u64 = 100;

/// Cap on the number of archive entries listed in the manifest.
pub const MAX_ARCHIVE_MEMBERS: usize = 1024;

/// Maximum recursion depth for nested archives. PR B does NOT recurse
/// (depth = 1), but the constant is here for PR C to honour.
///
/// Hardcoded: nested-archive recursion guard against zip-quine attacks.
/// Depth 2 is plenty for any real archive.
pub const MAX_ARCHIVE_DEPTH: u32 = 2;

// ── Output ──────────────────────────────────────────────────────────────────

/// Defensive cap on the number of `ContentBlock`s a single tool result
/// can carry. Until the budget guard is teach to sum block payloads
/// (TODO(pr-c)), this keeps the worst-case tool_result bounded.
pub const MAX_BLOCKS_PER_RESULT: usize = 20;

// ── Human-readable byte formatting ──────────────────────────────────────────

/// Format `bytes` as a short human-readable string ("1.2 MiB", "850 KiB",
/// "37 bytes"). Used in caption messages so the model and the TUI both
/// get a consistent unit.
pub fn human_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.0} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_units() {
        assert_eq!(human_bytes(0), "0 bytes");
        assert_eq!(human_bytes(512), "512 bytes");
        assert_eq!(human_bytes(2048), "2 KiB");
        assert_eq!(human_bytes(1024 * 1024 + 512 * 1024), "1.5 MiB");
        assert_eq!(human_bytes(2 * 1024 * 1024 * 1024), "2.0 GiB");
    }

    // Compile-time invariants — promoted out of a runtime test because
    // the values are const and clippy correctly flags `assert!(constant)`.
    // A regression on these would fail the build, not a test run.
    const _: () = {
        // Sanity: MAX_PDF_BYTES + MAX_IMAGE_BYTES must fit inside
        // MAX_FILE_BYTES with headroom — otherwise a single tool_result
        // could blow the per-file cap.
        assert!(MAX_PDF_BYTES + MAX_IMAGE_BYTES < MAX_FILE_BYTES);
        // The decompressed cap MUST exceed the compressed cap, otherwise
        // any archive that decompresses at all would be rejected.
        assert!(MAX_ARCHIVE_DECOMPRESSED > MAX_ARCHIVE_COMPRESSED);
    };
}
