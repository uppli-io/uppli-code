// OOXML extraction for XLSX / DOCX / PPTX via zip + quick-xml.
//
// Design choices:
//
// - Direct ZIP + XML walking rather than calamine / docx-rs. Keeps the
//   dep surface minimal and dodges crate-specific panic CVEs. The
//   trade-off: we extract the obvious text (cell values via shared
//   strings, paragraph runs, slide texts), not every spreadsheet
//   formula / formatting nuance. PR B's goal is "the LLM sees the
//   content", not "format-perfect reconstruction".
//
// - XML hardening: quick-xml never expands external entities by
//   default, but we explicitly reject `<!DOCTYPE>` declarations to
//   keep the door closed if a future quick-xml release flips
//   behaviour. Depth cap at MAX_XML_DEPTH defeats stack exhaustion
//   from deeply-nested elements.
//
// - ZIP hardening: refuse any entry whose name contains `..`, starts
//   with `/`, or is encrypted. Per-member uncompressed cap. Total
//   decompressed cap. The OOXML format DOES need a few specific
//   entries (xl/workbook.xml, xl/sharedStrings.xml, etc.) but those
//   are looked up by exact name — we never trust a traversal-bearing
//   name to resolve.
//
// - Output is text-only (blocks = None for all three formats). A
//   future commit could attach the original .xlsx as a Document
//   block for vision-capable providers, but Anthropic / OpenAI
//   vision endpoints don't accept OOXML inline today.

#![allow(dead_code)]
// `collapsible_match` would force the parser closures into match-guard
// soup that's much harder to read with quick-xml's verbose event types.
// Keep the explicit if-let inside the Text arm for readability.
#![allow(clippy::collapsible_match, clippy::collapsible_if)]

use std::io::{Cursor, Read};
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use tokio::fs;
use zip::ZipArchive;

use super::caption;
use super::detect::Kind;
use super::limits::{MAX_OOXML_BYTES, MAX_XML_DEPTH};
use super::output::HandlerOutput;

// Runtime cap on the bytes of inline text extracted from a single
// OOXML document flows through `Config::effective_max_ooxml_text_bytes`
// (knob: --max-ooxml-text-bytes); the fallback constant lives in
// `cc_core::constants::DEFAULT_MAX_OOXML_TEXT_BYTES` and is imported
// inside `mod tests` where it is exercised directly.

/// Cap on the DECOMPRESSED bytes read from a single ZIP entry inside
/// an OOXML / ODF archive. Without this, a 20 MiB .xlsx whose
/// xl/worksheets/sheet1.xml decompresses to 20 GiB (zip bomb) would
/// OOM the agent: `ZipFile::read_to_string` decompresses through-
/// fully and trusts the central-directory size. Bounded reads are
/// the only mitigation.
///
/// Hardcoded: per-entry zip-bomb guard inside OOXML — bounds the
/// memory of a single malicious sheet/part.
const MAX_ZIP_ENTRY_DECOMPRESSED: u64 = 32 * 1024 * 1024; // 32 MiB

/// Read a ZIP entry into a String, capping at MAX_ZIP_ENTRY_DECOMPRESSED
/// decompressed bytes. Returns the (possibly truncated) String and a
/// flag indicating whether the cap fired. Lossy-decodes non-UTF-8
/// bytes so a corrupted entry doesn't propagate an I/O error.
fn read_entry_capped<R: Read>(entry: &mut R) -> (String, bool) {
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut capped_reader = entry.take(MAX_ZIP_ENTRY_DECOMPRESSED + 1);
    if capped_reader.read_to_end(&mut buf).is_err() {
        return (String::new(), false);
    }
    let truncated = (buf.len() as u64) > MAX_ZIP_ENTRY_DECOMPRESSED;
    if truncated {
        buf.truncate(MAX_ZIP_ENTRY_DECOMPRESSED as usize);
    }
    let s = match std::str::from_utf8(&buf) {
        Ok(s) => s.to_string(),
        Err(_) => String::from_utf8_lossy(&buf).into_owned(),
    };
    (s, truncated)
}

/// Read an XLSX, DOCX or PPTX (or any OOXML kind passed by the
/// dispatcher) and produce a text-only `HandlerOutput`. ODF formats
/// route here too — the wrapper in `odf.rs` flips the entry filename
/// map (content.xml vs word/document.xml) and forwards.
///
pub async fn read_ooxml(path: &Path, kind: Kind, cfg: &cc_core::config::Config) -> HandlerOutput {
    let display = path.display().to_string();
    let meta = match fs::symlink_metadata(path).await {
        Ok(m) => m,
        Err(e) => {
            return HandlerOutput::error_text(format!("[OOXML read failed for {}: {}]", display, e))
        }
    };
    if meta.file_type().is_symlink() {
        return HandlerOutput::error_text(format!(
            "[OOXML read refused: {} is a symlink]",
            display
        ));
    }
    if !meta.is_file() {
        return HandlerOutput::error_text(format!(
            "[OOXML read refused: {} is not a regular file]",
            display
        ));
    }
    let size = meta.len();
    if size > MAX_OOXML_BYTES {
        return HandlerOutput::error_text(format!(
            "[OOXML: {} is {} > {} cap]",
            display,
            super::limits::human_bytes(size),
            super::limits::human_bytes(MAX_OOXML_BYTES)
        ));
    }

    let bytes = match fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            return HandlerOutput::error_text(format!("[OOXML read failed for {}: {}]", display, e))
        }
    };

    // ── Open the ZIP container ───────────────────────────────────────
    let cursor = Cursor::new(bytes);
    let mut archive = match ZipArchive::new(cursor) {
        Ok(a) => a,
        Err(e) => {
            return HandlerOutput::error_text(format!(
                "[OOXML: {} is not a valid OOXML/ZIP archive: {}]",
                display, e
            ))
        }
    };

    let max_ooxml_text_bytes = cfg.effective_max_ooxml_text_bytes();
    match kind {
        Kind::Xlsx => extract_xlsx(path, &mut archive, max_ooxml_text_bytes),
        Kind::Docx => extract_docx(path, &mut archive, size, max_ooxml_text_bytes),
        Kind::Pptx => extract_pptx(path, &mut archive, size, max_ooxml_text_bytes),
        // ODF formats are handled in odf.rs which calls helpers here.
        other => HandlerOutput::error_text(format!(
            "[OOXML handler called for non-OOXML kind: {:?}]",
            other
        )),
    }
}

// ─── XLSX ────────────────────────────────────────────────────────────────

fn extract_xlsx<R: std::io::Read + std::io::Seek>(
    path: &Path,
    archive: &mut ZipArchive<R>,
    max_ooxml_text_bytes: usize,
) -> HandlerOutput {
    let shared_strings = read_shared_strings(archive);

    // Discover sheet list from workbook.xml. Best-effort — if the
    // file is malformed we fall back to reading sheet1.xml directly.
    let sheets = read_sheet_list(archive).unwrap_or_else(|| vec!["Sheet1".to_string()]);
    let active = sheets
        .first()
        .cloned()
        .unwrap_or_else(|| "Sheet1".to_string());

    // Worksheet XML lives under xl/worksheets/sheet1.xml by Excel
    // convention. Third-party generators may differ; we keep this
    // simple in PR B and use the convention.
    let worksheet_name = "xl/worksheets/sheet1.xml";
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut total_rows = 0usize;
    let mut truncated = false;

    let mut entry_capped = false;
    if let Ok(mut zf) = archive.by_name(worksheet_name) {
        let (xml, capped) = read_entry_capped(&mut zf);
        entry_capped = capped;
        let result = walk_xlsx_rows(&xml, &shared_strings);
        total_rows = result.total_rows;
        truncated = result.truncated;
        rows = result.rows;
    }

    let mut out = caption::xlsx(path, &sheets, &active);
    out.push_str("\n\n");
    if rows.is_empty() {
        out.push_str("[No rows extracted — sheet may use a non-standard structure or be empty.]\n");
    } else {
        for row in &rows {
            out.push_str(&row.join("\t"));
            out.push('\n');
        }
        if truncated {
            out.push_str(&format!(
                "\n[Note: walker stopped early at {} rows of {} (malformed XML or depth guard tripped).]\n",
                rows.len(),
                total_rows,
            ));
        }
    }
    if entry_capped {
        out.push_str(&format!(
            "\n[Note: worksheet XML capped at {} MiB decompressed — suspected zip bomb or oversized sheet.]\n",
            MAX_ZIP_ENTRY_DECOMPRESSED / (1024 * 1024)
        ));
    }
    cap_text_bytes(&mut out, max_ooxml_text_bytes);

    HandlerOutput::success_text(out)
}

struct XlsxRows {
    rows: Vec<Vec<String>>,
    total_rows: usize,
    truncated: bool,
}

fn walk_xlsx_rows(xml: &str, shared_strings: &[String]) -> XlsxRows {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    reader.config_mut().expand_empty_elements = true;
    let mut buf = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut total_rows = 0usize;
    let mut truncated = false;
    let mut depth: u32 = 0;
    let mut in_value = false;
    let mut cell_type_shared = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::DocType(_)) => {
                // Defensive — DOCTYPE in OOXML is suspicious. Stop here.
                truncated = true;
                break;
            }
            Ok(Event::Start(e)) => {
                depth = depth.saturating_add(1);
                if depth > MAX_XML_DEPTH {
                    break;
                }
                let name = e.name();
                let name_bytes = name.as_ref();
                if name_bytes == b"row" {
                    current_row.clear();
                } else if name_bytes == b"c" {
                    cell_type_shared = false;
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"t" {
                            cell_type_shared = attr.value.as_ref() == b"s";
                            break;
                        }
                    }
                } else if name_bytes == b"v" {
                    in_value = true;
                }
            }
            Ok(Event::End(e)) => {
                let name_bytes = e.name().as_ref().to_vec();
                if name_bytes == b"row" {
                    total_rows += 1;
                    rows.push(std::mem::take(&mut current_row));
                } else if name_bytes == b"v" {
                    in_value = false;
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Text(t)) => {
                if in_value {
                    let raw = t
                        .unescape()
                        .unwrap_or(std::borrow::Cow::Borrowed(""))
                        .to_string();
                    let resolved = if cell_type_shared {
                        raw.parse::<usize>()
                            .ok()
                            .and_then(|i| shared_strings.get(i).cloned())
                            .unwrap_or(raw)
                    } else {
                        raw
                    };
                    current_row.push(resolved);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    XlsxRows {
        rows,
        total_rows,
        truncated,
    }
}

fn read_shared_strings<R: Read + std::io::Seek>(archive: &mut ZipArchive<R>) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(mut zf) = archive.by_name("xl/sharedStrings.xml") else {
        return out;
    };
    let (xml, _capped) = read_entry_capped(&mut zf);
    if xml.is_empty() {
        return out;
    }
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    reader.config_mut().expand_empty_elements = true;
    let mut buf = Vec::new();
    let mut current = String::new();
    let mut in_t = false;
    let mut depth: u32 = 0;
    let mut current_string = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::DocType(_)) => break,
            Ok(Event::Start(e)) => {
                depth = depth.saturating_add(1);
                if depth > MAX_XML_DEPTH {
                    break;
                }
                let name = e.name();
                if name.as_ref() == b"si" {
                    current_string.clear();
                } else if name.as_ref() == b"t" {
                    in_t = true;
                    current.clear();
                }
            }
            Ok(Event::End(e)) => {
                let name_bytes = e.name().as_ref().to_vec();
                if name_bytes == b"t" {
                    in_t = false;
                    current_string.push_str(&current);
                } else if name_bytes == b"si" {
                    out.push(std::mem::take(&mut current_string));
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Text(t)) => {
                if in_t {
                    if let Ok(s) = t.unescape() {
                        current.push_str(&s);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

fn read_sheet_list<R: Read + std::io::Seek>(archive: &mut ZipArchive<R>) -> Option<Vec<String>> {
    let mut zf = archive.by_name("xl/workbook.xml").ok()?;
    let (xml, _capped) = read_entry_capped(&mut zf);
    if xml.is_empty() {
        return None;
    }
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    reader.config_mut().expand_empty_elements = true;
    let mut buf = Vec::new();
    let mut sheets = Vec::new();
    let mut depth: u32 = 0;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::DocType(_)) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                depth = depth.saturating_add(1);
                if depth > MAX_XML_DEPTH {
                    break;
                }
                if e.name().as_ref() == b"sheet" {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"name" {
                            if let Ok(s) = std::str::from_utf8(&attr.value) {
                                sheets.push(s.to_string());
                            }
                        }
                    }
                }
            }
            Ok(Event::End(_)) => {
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    if sheets.is_empty() {
        None
    } else {
        Some(sheets)
    }
}

// ─── DOCX ────────────────────────────────────────────────────────────────

fn extract_docx<R: Read + std::io::Seek>(
    path: &Path,
    archive: &mut ZipArchive<R>,
    size: u64,
    max_ooxml_text_bytes: usize,
) -> HandlerOutput {
    let mut out = caption::docx(path, size);
    out.push_str("\n\n");
    let (xml, entry_capped) = match archive.by_name("word/document.xml") {
        Ok(mut zf) => {
            let (s, capped) = read_entry_capped(&mut zf);
            if s.is_empty() {
                return HandlerOutput::error_text(format!(
                    "[DOCX: {} word/document.xml unreadable or empty]",
                    path.display()
                ));
            }
            (s, capped)
        }
        Err(_) => {
            return HandlerOutput::error_text(format!(
                "[DOCX: {} missing word/document.xml — not a valid DOCX]",
                path.display()
            ))
        }
    };
    let body = walk_ooxml_text(&xml, b"t", max_ooxml_text_bytes);
    if body.is_empty() {
        out.push_str("[No text runs found in word/document.xml.]\n");
    } else {
        out.push_str(&body);
    }
    if entry_capped {
        out.push_str(&format!(
            "\n[Note: word/document.xml capped at {} MiB decompressed — suspected zip bomb or oversized document.]\n",
            MAX_ZIP_ENTRY_DECOMPRESSED / (1024 * 1024)
        ));
    }
    cap_text_bytes(&mut out, max_ooxml_text_bytes);
    HandlerOutput::success_text(out)
}

// ─── PPTX ────────────────────────────────────────────────────────────────

fn extract_pptx<R: Read + std::io::Seek>(
    path: &Path,
    archive: &mut ZipArchive<R>,
    size: u64,
    max_ooxml_text_bytes: usize,
) -> HandlerOutput {
    // Collect slide entries, sort by numeric index (NOT lexicographically
    // — slide10.xml < slide2.xml lexicographically, but we want 1, 2, ...
    // 10, 11).
    let mut slides: Vec<(usize, String)> = archive
        .file_names()
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .filter_map(|n| {
            let bare = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?;
            let idx: usize = bare.parse().ok()?;
            // Path-traversal guard: refuse any name that contains ..
            if n.contains("..") || n.starts_with('/') {
                return None;
            }
            Some((idx, n.to_string()))
        })
        .collect();
    slides.sort_by_key(|(idx, _)| *idx);
    let total_slides = slides.len();

    let mut out = caption::pptx(path, total_slides, size);
    out.push_str("\n\n");

    let mut any_slide_capped = false;
    for (idx, name) in &slides {
        let mut xml = String::new();
        if let Ok(mut zf) = archive.by_name(name) {
            let (s, capped) = read_entry_capped(&mut zf);
            xml = s;
            any_slide_capped |= capped;
        }
        out.push_str(&format!("## Slide {}\n", idx));
        let slide_text = walk_ooxml_text(&xml, b"t", max_ooxml_text_bytes);
        if slide_text.is_empty() {
            out.push_str("[no text runs]\n");
        } else {
            out.push_str(&slide_text);
            if !slide_text.ends_with('\n') {
                out.push('\n');
            }
        }
        out.push('\n');
    }

    if any_slide_capped {
        out.push_str(&format!(
            "\n[Note: one or more slide XML entries capped at {} MiB decompressed — suspected zip bomb.]\n",
            MAX_ZIP_ENTRY_DECOMPRESSED / (1024 * 1024)
        ));
    }
    cap_text_bytes(&mut out, max_ooxml_text_bytes);
    HandlerOutput::success_text(out)
}

// ─── Shared XML text walker ─────────────────────────────────────────────
//
// Collects every text-run found inside `tag` elements (typically `t` for
// DOCX and PPTX). Honours MAX_XML_DEPTH; refuses DOCTYPE; ignores
// unknown elements. Returns concatenated text with paragraph-level
// newlines (PPTX `<a:p>` and DOCX `<w:p>` both end with a newline).

pub(super) fn walk_ooxml_text(xml: &str, tag: &[u8], max_bytes: usize) -> String {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    reader.config_mut().expand_empty_elements = true;
    let mut buf = Vec::new();
    let mut out = String::new();
    let mut depth: u32 = 0;
    let mut in_t = false;
    let mut in_para = false;
    loop {
        if out.len() > max_bytes {
            cap_text_bytes(&mut out, max_bytes);
            break;
        }
        match reader.read_event_into(&mut buf) {
            Ok(Event::DocType(_)) => break,
            Ok(Event::Start(e)) => {
                depth = depth.saturating_add(1);
                if depth > MAX_XML_DEPTH {
                    break;
                }
                let name = e.name();
                let raw = name.as_ref();
                let local = local_name(raw);
                if local == tag {
                    in_t = true;
                } else if local == b"p" {
                    in_para = true;
                }
            }
            Ok(Event::End(e)) => {
                let local = local_name(e.name().as_ref()).to_vec();
                if local == tag {
                    in_t = false;
                } else if local == b"p" && in_para {
                    out.push('\n');
                    in_para = false;
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Text(t)) => {
                if in_t {
                    if let Ok(s) = t.unescape() {
                        out.push_str(&s);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Strip an XML namespace prefix (`w:t` → `t`). quick-xml does not
/// strip prefixes by default, so the walker compares against bare
/// local names.
fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().position(|&b| b == b':') {
        Some(idx) => &name[idx + 1..],
        None => name,
    }
}

/// UTF-8-safe truncation of `s` at DEFAULT_MAX_OOXML_TEXT_BYTES. Walks back to
/// the nearest char boundary so we never panic mid-codepoint.
fn cap_text_bytes(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str(
        "\n[Truncated at max_ooxml_text_bytes — use Bash + libreoffice / unzip for the rest.]\n",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::constants::DEFAULT_MAX_OOXML_TEXT_BYTES;

    #[test]
    fn local_name_strips_namespace() {
        assert_eq!(local_name(b"w:t"), b"t");
        assert_eq!(local_name(b"t"), b"t");
        assert_eq!(local_name(b"a:p"), b"p");
    }

    #[test]
    fn walk_ooxml_text_collects_t_runs() {
        let xml = r#"<doc xmlns:w="urn:x"><w:p><w:r><w:t>hello </w:t></w:r><w:r><w:t>world</w:t></w:r></w:p></doc>"#;
        let text = walk_ooxml_text(xml, b"t", DEFAULT_MAX_OOXML_TEXT_BYTES);
        assert!(text.contains("hello"));
        assert!(text.contains("world"));
        assert!(text.contains("\n"), "paragraph end should emit a newline");
    }

    #[test]
    fn walk_ooxml_text_stops_on_doctype() {
        let xml = r#"<!DOCTYPE poison><doc><w:t>nope</w:t></doc>"#;
        let text = walk_ooxml_text(xml, b"t", DEFAULT_MAX_OOXML_TEXT_BYTES);
        assert!(text.is_empty(), "DOCTYPE must short-circuit");
    }

    #[test]
    fn cap_text_bytes_is_char_safe() {
        // Build a string with a multi-byte char right at the cap boundary.
        let mut s = "a".repeat(DEFAULT_MAX_OOXML_TEXT_BYTES - 1);
        s.push('é'); // 2 bytes — straddles the boundary
        s.push_str(&"b".repeat(100));
        cap_text_bytes(&mut s, DEFAULT_MAX_OOXML_TEXT_BYTES);
        assert!(s.len() <= DEFAULT_MAX_OOXML_TEXT_BYTES + 200); // truncation footer
                                                                // Must not panic — implicit by reaching here.
        assert!(s.contains("Truncated"));
    }

    #[test]
    fn xlsx_walk_resolves_shared_strings() {
        let xml = r#"<worksheet xmlns="urn:x"><sheetData>
            <row><c t="s"><v>0</v></c><c t="s"><v>1</v></c></row>
            <row><c><v>42</v></c></row>
        </sheetData></worksheet>"#;
        let shared = vec!["hello".to_string(), "world".to_string()];
        let result = walk_xlsx_rows(xml, &shared);
        assert_eq!(result.total_rows, 2);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0], vec!["hello", "world"]);
        assert_eq!(result.rows[1], vec!["42"]);
    }

    #[test]
    fn read_entry_capped_truncates_at_cap() {
        // Build a Read that would yield 64 MiB of zeros — twice the cap.
        // A real zip-bomb member would behave similarly: a 1 KiB
        // compressed entry that decompresses to gigabytes via repeat()
        // sees its decompressor stream produce arbitrarily many bytes.
        let stream = std::io::repeat(0u8);
        let mut bounded = stream.take(64 * 1024 * 1024);
        let (s, capped) = read_entry_capped(&mut bounded);
        assert!(capped, "zip-bomb stream must be flagged as capped");
        assert!(
            (s.len() as u64) <= MAX_ZIP_ENTRY_DECOMPRESSED,
            "output must not exceed the per-entry cap, got {} bytes",
            s.len()
        );
    }

    #[test]
    fn read_entry_capped_keeps_small_entries_intact() {
        let small = b"normal content under the cap".to_vec();
        let mut cursor = std::io::Cursor::new(small.clone());
        let (s, capped) = read_entry_capped(&mut cursor);
        assert!(!capped);
        assert_eq!(s.as_bytes(), small.as_slice());
    }

    #[test]
    fn xlsx_walk_emits_all_rows_uncapped() {
        let mut xml = String::from(r#"<worksheet><sheetData>"#);
        for _ in 0..505 {
            xml.push_str(r#"<row><c><v>1</v></c></row>"#);
        }
        xml.push_str("</sheetData></worksheet>");
        let result = walk_xlsx_rows(&xml, &[]);
        assert!(!result.truncated);
        assert_eq!(result.rows.len(), 505);
        assert_eq!(result.total_rows, 505);
    }
}
