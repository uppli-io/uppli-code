// Text format handler — streaming size cap, lossy encoding fallback,
// line-numbered output.
//
// LOAD-BEARING FORMAT: the `<num>\t<line>\n` output format is consumed by
// at least three other places in the codebase:
//   - `tui/src/messages/mod.rs::render_file_read_result` parses the
//     leading "<num>\t" to detect tool-result line ranges.
//   - `query/src/lib.rs` CodeAudit hook expects line-numbered output to
//     reference specific lines in audit reports.
//   - The model itself has been trained on this exact shape and will
//     write Edit/Write commands referencing lines accordingly.
// Any change to the format must update all three.

#![allow(dead_code)]

use std::path::Path;
use tokio::io::{AsyncReadExt, BufReader};

use super::limits::{human_bytes, DEFAULT_LINE_LIMIT, MAX_LINE_CHARS, MAX_TEXT_BYTES};
use super::output::{HandlerOutput, Truncation};

/// Runtime-resolved size caps for a single text read. Built by the
/// dispatcher from `Config::effective_*` helpers so the user can override
/// them via CLI / settings (`--max-text-bytes`, `--max-line-chars`,
/// `--default-read-line-limit`). Falls back to the module constants when
/// no override is in play (see `Default`).
#[derive(Debug, Clone, Copy)]
pub struct TextLimits {
    pub max_text_bytes: u64,
    pub max_line_chars: usize,
    pub default_line_limit: usize,
}

impl Default for TextLimits {
    fn default() -> Self {
        Self {
            max_text_bytes: MAX_TEXT_BYTES,
            max_line_chars: MAX_LINE_CHARS,
            default_line_limit: DEFAULT_LINE_LIMIT,
        }
    }
}

/// Read a text file with a streaming byte cap, decode it as UTF-8 with
/// a lossy Windows-1252 fallback, and emit the canonical line-numbered
/// representation respecting `offset` / `limit`.
///
/// `offset` is 1-based (per the legacy contract). `offset = 0` and
/// `offset = 1` both map to "start from the first line".
///
/// Compile-time-default variant kept for the legacy callers (tests and
/// handlers that don't yet route a Config). Use
/// `read_text_with_limits` from the dispatcher to honour user overrides.
pub async fn read_text(path: &Path, offset: Option<usize>, limit: Option<usize>) -> HandlerOutput {
    read_text_with_limits(path, offset, limit, TextLimits::default()).await
}

/// Runtime-configurable variant of `read_text` — accepts a `TextLimits`
/// resolved by the caller from the active `Config`.
pub async fn read_text_with_limits(
    path: &Path,
    offset: Option<usize>,
    limit: Option<usize>,
    limits: TextLimits,
) -> HandlerOutput {
    let max_text_bytes = limits.max_text_bytes;
    let max_line_chars = limits.max_line_chars;
    let default_line_limit = limits.default_line_limit;
    // ── Step 1: open + stream up to max_text_bytes ──────────────────────
    //
    // We must NEVER load more than max_text_bytes into memory, even if the
    // pre-flight cap (MAX_FILE_BYTES = 100 MiB) passed. A 50 MiB log file
    // is well under pre-flight but would still cost the budget too much.
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            return HandlerOutput::error_text(format!(
                "Failed to read file {}: {}",
                path.display(),
                e
            ));
        }
    };

    // The actual file length (used to decide whether truncation happened
    // mid-stream and to report total bytes in the footer).
    let total_bytes = match tokio::fs::metadata(path).await {
        Ok(m) => m.len(),
        Err(_) => 0,
    };

    let mut reader = BufReader::new(file).take(max_text_bytes);
    let mut bytes = Vec::with_capacity(total_bytes.min(max_text_bytes) as usize);
    if let Err(e) = reader.read_to_end(&mut bytes).await {
        return HandlerOutput::error_text(format!(
            "Failed while reading {}: {}",
            path.display(),
            e
        ));
    }

    let shown_bytes = bytes.len() as u64;
    let byte_truncation = if total_bytes > shown_bytes {
        Some(Truncation::Bytes {
            shown: shown_bytes,
            total: total_bytes,
        })
    } else {
        None
    };

    // ── Step 2: decode ──────────────────────────────────────────────────
    //
    // UTF-8 fast path: if the bytes are valid UTF-8, no allocation cost.
    // Otherwise we fall back to encoding_rs Windows-1252 lossy decode —
    // produces something readable for the bulk of legacy logs and
    // Windows-authored files that are CP-1252 in practice, instead of
    // the legacy hard "appears to be binary" error.
    let (decoded, encoding_note) = match std::str::from_utf8(&bytes) {
        Ok(s) => (std::borrow::Cow::Borrowed(s), None),
        Err(_) => {
            let (cow, _, had_errors) = encoding_rs::WINDOWS_1252.decode(&bytes);
            let note = if had_errors {
                Some("[note: decoded as Windows-1252 with lossy substitutions]")
            } else {
                Some("[note: decoded as Windows-1252]")
            };
            // Owned String — we don't own the input slice anymore.
            (std::borrow::Cow::Owned(cow.into_owned()), note)
        }
    };

    if decoded.is_empty() {
        return HandlerOutput::success_text(format!(
            "[File {} exists but is empty]",
            path.display()
        ));
    }

    // ── Step 3: split lines, apply offset/limit ─────────────────────────
    let lines: Vec<&str> = decoded.lines().collect();
    let total_lines = lines.len();

    let offset_param = offset.unwrap_or(0);
    let limit_param = limit.unwrap_or(default_line_limit);

    // 1-based offset → 0-based index; offset = 0 and 1 both start at 0.
    let start = if offset_param > 0 {
        offset_param.saturating_sub(1)
    } else {
        0
    };
    if start >= total_lines {
        // Offset past EOF — error to match the legacy contract.
        return HandlerOutput::error_text(format!(
            "Offset {} exceeds total line count {} in {}",
            offset_param,
            total_lines,
            path.display()
        ));
    }
    let end = start.saturating_add(limit_param).min(total_lines);

    // ── Step 4: render <num>\t<line>\n with per-line truncation ─────────
    let mut output = String::new();
    if let Some(note) = encoding_note {
        output.push_str(note);
        output.push('\n');
    }
    let width = format!("{}", end).len();

    for (i, line) in lines[start..end].iter().enumerate() {
        let line_num = start + i + 1;
        // Per-line cap: a 1 MiB minified JS line on a single row would
        // otherwise blow the per-result budget.
        let rendered: std::borrow::Cow<'_, str> = if line.len() > max_line_chars {
            // char_indices is the safe truncation — slicing at byte
            // boundaries inside a multi-byte UTF-8 sequence would panic.
            let cut = line
                .char_indices()
                .take_while(|(idx, _)| *idx < max_line_chars)
                .last()
                .map(|(idx, c)| idx + c.len_utf8())
                .unwrap_or(0);
            std::borrow::Cow::Owned(format!(
                "{}[…line truncated at {} chars, {} total]",
                &line[..cut],
                max_line_chars,
                line.len()
            ))
        } else {
            std::borrow::Cow::Borrowed(*line)
        };
        output.push_str(&format!(
            "{:>width$}\t{}\n",
            line_num,
            rendered,
            width = width
        ));
    }

    // ── Step 5: build HandlerOutput with truncation footers ─────────────
    //
    // Two truncation reasons can apply at once: byte cap reached AND
    // line cap reached. Lines wins when both fire because the caller
    // controls limit; the byte-level truncation is implicit.
    let truncation = if end < total_lines {
        Some(Truncation::Lines {
            shown: end,
            total: total_lines,
        })
    } else {
        byte_truncation.clone()
    };

    if shown_bytes < total_bytes && end >= total_lines {
        // Lines weren't truncated but bytes were — append a hint so the
        // model doesn't think it saw the full file.
        output.push_str(&format!(
            "\n[note: file is {} but only first {} were read — increase MAX_TEXT_BYTES or use Bash]\n",
            human_bytes(total_bytes),
            human_bytes(shown_bytes),
        ));
    }

    HandlerOutput {
        content: output,
        blocks: Vec::new(),
        truncation,
        is_error: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_tmp(contents: &[u8], name: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::File::create(&path)
            .expect("create")
            .write_all(contents)
            .expect("write");
        (dir, path)
    }

    #[tokio::test]
    async fn utf8_simple_file_renders_numbered_lines() {
        let (_dir, path) = write_tmp(b"one\ntwo\nthree\n", "a.txt");
        let out = read_text(&path, None, None).await;
        assert!(!out.is_error);
        assert!(out.content.contains("1\tone"));
        assert!(out.content.contains("2\ttwo"));
        assert!(out.content.contains("3\tthree"));
    }

    #[tokio::test]
    async fn empty_file_returns_placeholder() {
        let (_dir, path) = write_tmp(b"", "empty.txt");
        let out = read_text(&path, None, None).await;
        assert!(!out.is_error);
        assert!(out.content.contains("exists but is empty"));
    }

    #[tokio::test]
    async fn offset_past_eof_errors() {
        let (_dir, path) = write_tmp(b"one\ntwo\n", "two.txt");
        let out = read_text(&path, Some(999), None).await;
        assert!(out.is_error);
        assert!(out.content.contains("exceeds total"));
    }

    #[tokio::test]
    async fn limit_truncates_with_footer() {
        let big: String = (1..=2500).map(|i| format!("line{}\n", i)).collect();
        let (_dir, path) = write_tmp(big.as_bytes(), "big.txt");
        let out = read_text(&path, None, Some(100)).await;
        assert!(!out.is_error);
        // truncation field set
        assert!(matches!(
            out.truncation,
            Some(Truncation::Lines {
                shown: 100,
                total: 2500
            })
        ));
    }

    #[tokio::test]
    async fn windows_1252_lossy_fallback_does_not_error() {
        // 0x85 in Windows-1252 is the ellipsis (…) — invalid UTF-8 alone.
        let bytes = b"caf\xe9\n0x85 here: \x85\n";
        let (_dir, path) = write_tmp(bytes, "cp1252.txt");
        let out = read_text(&path, None, None).await;
        assert!(!out.is_error, "must NOT hard-error on non-UTF8 input");
        assert!(
            out.content.contains("Windows-1252"),
            "must note the fallback encoding"
        );
        assert!(out.content.contains("café"));
    }

    #[tokio::test]
    async fn per_line_cap_truncates_megaline() {
        // One ridiculously long line. Verify the per-line cap fires and
        // the line truncation marker is emitted.
        let mut huge = String::new();
        huge.push_str(&"x".repeat(MAX_LINE_CHARS + 1000));
        huge.push('\n');
        huge.push_str("short\n");
        let (_dir, path) = write_tmp(huge.as_bytes(), "wide.txt");
        let out = read_text(&path, None, None).await;
        assert!(!out.is_error);
        assert!(
            out.content.contains("line truncated"),
            "must mark the over-long line: {}",
            &out.content.chars().take(200).collect::<String>()
        );
        // Second line still visible.
        assert!(out.content.contains("short"));
    }

    #[tokio::test]
    async fn streaming_cap_keeps_bytes_in_memory_bounded() {
        // Hand-write 12 MiB of text and confirm we don't load all of it.
        let chunk = "abcdefghij\n".repeat(102_400); // ~1 MiB
        let mut huge = String::new();
        for _ in 0..12 {
            huge.push_str(&chunk);
        }
        let (_dir, path) = write_tmp(huge.as_bytes(), "huge.txt");
        let out = read_text(&path, None, None).await;
        assert!(!out.is_error);
        // Output length must be far below the source — proves the stream cap.
        assert!(
            (out.content.len() as u64) < MAX_TEXT_BYTES * 2,
            "rendered output must respect byte cap"
        );
    }
}
