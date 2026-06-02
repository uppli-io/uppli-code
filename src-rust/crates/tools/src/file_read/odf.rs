// ODF (OpenDocument Format) extraction for ODT / ODS / ODP.
//
// ODF documents are ZIP archives carrying a `content.xml` payload with
// the same `<text:p>` / `<text:span>` / `<table:table-cell>` shape
// across all three formats. Rather than duplicate the XML walking
// logic in ooxml.rs, we reuse `ooxml::walk_ooxml_text` with a different
// tag list.

#![allow(dead_code)]

use std::io::{Cursor, Read};
use std::path::Path;

use tokio::fs;
use zip::ZipArchive;

/// Decompressed-bytes cap per ODF ZIP entry. Mirrors the OOXML guard
/// so a malicious .odt whose content.xml claims 1 GiB doesn't OOM.
const MAX_ODF_ENTRY_DECOMPRESSED: u64 = 32 * 1024 * 1024;

fn read_entry_capped<R: Read>(entry: &mut R) -> (String, bool) {
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut capped_reader = entry.take(MAX_ODF_ENTRY_DECOMPRESSED + 1);
    if capped_reader.read_to_end(&mut buf).is_err() {
        return (String::new(), false);
    }
    let truncated = (buf.len() as u64) > MAX_ODF_ENTRY_DECOMPRESSED;
    if truncated {
        buf.truncate(MAX_ODF_ENTRY_DECOMPRESSED as usize);
    }
    let s = match std::str::from_utf8(&buf) {
        Ok(s) => s.to_string(),
        Err(_) => String::from_utf8_lossy(&buf).into_owned(),
    };
    (s, truncated)
}

use super::caption;
use super::detect::Kind;
use super::limits::{human_bytes, MAX_OOXML_BYTES};
use super::output::HandlerOutput;

pub async fn read_odf(path: &Path, kind: Kind) -> HandlerOutput {
    let display = path.display().to_string();
    let meta = match fs::symlink_metadata(path).await {
        Ok(m) => m,
        Err(e) => {
            return HandlerOutput::error_text(format!("[ODF read failed for {}: {}]", display, e))
        }
    };
    if meta.file_type().is_symlink() {
        return HandlerOutput::error_text(format!("[ODF read refused: {} is a symlink]", display));
    }
    if !meta.is_file() {
        return HandlerOutput::error_text(format!(
            "[ODF read refused: {} is not a regular file]",
            display
        ));
    }
    let size = meta.len();
    if size > MAX_OOXML_BYTES {
        return HandlerOutput::error_text(format!(
            "[ODF: {} is {} > {} cap]",
            display,
            human_bytes(size),
            human_bytes(MAX_OOXML_BYTES)
        ));
    }
    let bytes = match fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            return HandlerOutput::error_text(format!("[ODF read failed for {}: {}]", display, e))
        }
    };
    let cursor = Cursor::new(bytes);
    let mut archive = match ZipArchive::new(cursor) {
        Ok(a) => a,
        Err(e) => {
            return HandlerOutput::error_text(format!(
                "[ODF: {} is not a valid ODF/ZIP archive: {}]",
                display, e
            ))
        }
    };

    let (xml, entry_capped) = match archive.by_name("content.xml") {
        Ok(mut zf) => {
            let (s, capped) = read_entry_capped(&mut zf);
            if s.is_empty() {
                return HandlerOutput::error_text(format!(
                    "[ODF: {} content.xml unreadable or empty]",
                    display
                ));
            }
            (s, capped)
        }
        Err(_) => {
            return HandlerOutput::error_text(format!(
                "[ODF: {} missing content.xml — not a valid ODF document]",
                display
            ));
        }
    };

    // ODF uses `<text:p>` runs with the local tag `p`. The same walker
    // that handles DOCX `<w:t>` doesn't fit perfectly here — ODF text
    // is inside `<text:span>` and other containers. As a pragmatic
    // first pass we treat every text-node inside the content.xml as
    // body text, with newlines on paragraph boundaries.
    let text = super::ooxml::walk_ooxml_text(&xml, b"span");
    let combined = if text.is_empty() {
        // Fallback: pull every text node by reusing the walker but
        // matching the `p` tag (paragraph text directly inside <text:p>).
        super::ooxml::walk_ooxml_text(&xml, b"p")
    } else {
        text
    };

    let mut out = caption::stub(
        path,
        &format!("ODF {}", kind.label()),
        "Showing extracted text. Use Bash + libreoffice for layout fidelity.",
    );
    out.push_str("\n\n");
    if combined.is_empty() {
        out.push_str("[No text extracted — document may use a non-standard ODF structure.]\n");
    } else {
        out.push_str(&combined);
    }
    if entry_capped {
        out.push_str(&format!(
            "\n[Note: content.xml capped at {} MiB decompressed — suspected zip bomb or oversized document.]\n",
            MAX_ODF_ENTRY_DECOMPRESSED / (1024 * 1024)
        ));
    }
    HandlerOutput::success_text(out)
}
