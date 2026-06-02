// Magic-byte + extension sniffer.
//
// Sniff order: magic-bytes first, extension as fallback. Magic wins
// because a `.txt` file that is actually a PDF should be routed to the
// PDF handler with a `[detected as PDF]` note prepended.
//
// We deliberately hand-roll the dispatch table instead of pulling
// `infer` or `mime_guess`: this code is hot on every read, the magic
// byte sequences we care about fit in one screen, and a third-party
// crate would force us to bridge its taxonomy into ours.

#![allow(dead_code)]

use std::path::Path;

/// One of the formats the FileRead tool can dispatch on. Every variant
/// maps to a handler module in this directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    // ── Text and structured text ─────────────────────────────────────
    Text,
    Csv,
    Tsv,
    Json,
    Jsonl,
    Xml,
    Html,
    Markdown,
    Notebook,
    // ── Image (base64 inlined) ───────────────────────────────────────
    ImagePng,
    ImageJpeg,
    ImageGif,
    ImageWebp,
    ImageBmp,
    ImageIco,
    Svg, // routed to text handler
    // ── Document ─────────────────────────────────────────────────────
    Pdf,
    // ── OOXML (Office Open XML) ──────────────────────────────────────
    Xlsx,
    Docx,
    Pptx,
    // ── ODF (OpenDocument Format) ────────────────────────────────────
    Ods,
    Odt,
    Odp,
    // ── Legacy binary Office (graceful stub) ─────────────────────────
    LegacyXls,
    LegacyDoc,
    LegacyPpt,
    // ── Archives ─────────────────────────────────────────────────────
    Zip,
    TarPlain,
    TarGz,
    TarBz2, // stub (no bz2 dep in PR B)
    TarXz,  // stub (no xz dep in PR B)
    TarZst, // stub (no zstd dep in PR B)
    SevenZ, // stub (no 7z dep in PR B)
    Rar,    // stub (no rar dep in PR B)
    // ── Catch-all ────────────────────────────────────────────────────
    Unknown,
}

impl Kind {
    /// Canonical media_type string per RFC 2046 / IANA registry. Used
    /// by `image.rs` and `pdf.rs` to populate `ImageSource.media_type`
    /// and `DocumentSource.media_type` consistently with what the
    /// OpenAI vision wire format expects (see openai_provider.rs
    /// helpers added in PR A commit 6).
    pub fn media_type(self) -> Option<&'static str> {
        match self {
            Kind::ImagePng => Some("image/png"),
            Kind::ImageJpeg => Some("image/jpeg"),
            Kind::ImageGif => Some("image/gif"),
            Kind::ImageWebp => Some("image/webp"),
            Kind::ImageBmp => Some("image/bmp"),
            Kind::ImageIco => Some("image/vnd.microsoft.icon"),
            Kind::Svg => Some("image/svg+xml"),
            Kind::Pdf => Some("application/pdf"),
            _ => None,
        }
    }

    /// Human-readable label used in captions and stubs.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Text => "Text",
            Kind::Csv => "CSV",
            Kind::Tsv => "TSV",
            Kind::Json => "JSON",
            Kind::Jsonl => "JSONL",
            Kind::Xml => "XML",
            Kind::Html => "HTML",
            Kind::Markdown => "Markdown",
            Kind::Notebook => "Jupyter Notebook",
            Kind::ImagePng => "PNG",
            Kind::ImageJpeg => "JPEG",
            Kind::ImageGif => "GIF",
            Kind::ImageWebp => "WebP",
            Kind::ImageBmp => "BMP",
            Kind::ImageIco => "ICO",
            Kind::Svg => "SVG",
            Kind::Pdf => "PDF",
            Kind::Xlsx => "XLSX",
            Kind::Docx => "DOCX",
            Kind::Pptx => "PPTX",
            Kind::Ods => "ODS",
            Kind::Odt => "ODT",
            Kind::Odp => "ODP",
            Kind::LegacyXls => "Legacy XLS",
            Kind::LegacyDoc => "Legacy DOC",
            Kind::LegacyPpt => "Legacy PPT",
            Kind::Zip => "ZIP",
            Kind::TarPlain => "TAR",
            Kind::TarGz => "TAR.GZ",
            Kind::TarBz2 => "TAR.BZ2",
            Kind::TarXz => "TAR.XZ",
            Kind::TarZst => "TAR.ZST",
            Kind::SevenZ => "7Z",
            Kind::Rar => "RAR",
            Kind::Unknown => "unknown",
        }
    }

    /// True when this format may carry an Image / Document block on
    /// vision-capable providers. Used by the dispatcher to short-circuit
    /// the head-sniff for kinds that never need a probe.
    pub fn carries_visual_payload(self) -> bool {
        matches!(
            self,
            Kind::ImagePng
                | Kind::ImageJpeg
                | Kind::ImageGif
                | Kind::ImageWebp
                | Kind::ImageBmp
                | Kind::Pdf
        )
    }
}

/// Sniff a file's format from (a) its leading bytes and (b) its
/// extension. Magic bytes take priority; the extension is consulted
/// only when magic is inconclusive.
///
/// `head` should be the first ~4 KiB of the file — enough to capture
/// every magic signature in this dispatch table.
pub fn sniff(path: &Path, head: &[u8]) -> Kind {
    if let Some(k) = sniff_by_magic(head) {
        return k;
    }
    sniff_by_extension(path)
}

/// Pure magic-byte detection. Returns `None` when no signature matches.
fn sniff_by_magic(head: &[u8]) -> Option<Kind> {
    // ── PDF: %PDF- ──────────────────────────────────────────────────
    if head.starts_with(b"%PDF-") {
        return Some(Kind::Pdf);
    }
    // ── PNG: 89 50 4E 47 0D 0A 1A 0A ─────────────────────────────────
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Kind::ImagePng);
    }
    // ── JPEG: FF D8 FF ──────────────────────────────────────────────
    if head.starts_with(b"\xff\xd8\xff") {
        return Some(Kind::ImageJpeg);
    }
    // ── GIF: GIF87a / GIF89a ────────────────────────────────────────
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some(Kind::ImageGif);
    }
    // ── WebP: RIFF....WEBP ──────────────────────────────────────────
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return Some(Kind::ImageWebp);
    }
    // ── BMP: BM ─────────────────────────────────────────────────────
    if head.starts_with(b"BM") && head.len() >= 14 {
        return Some(Kind::ImageBmp);
    }
    // ── ICO: 00 00 01 00 ────────────────────────────────────────────
    if head.starts_with(b"\x00\x00\x01\x00") {
        return Some(Kind::ImageIco);
    }
    // ── ZIP: PK\x03\x04 (also OOXML / ODF / JAR / EPUB) ─────────────
    if head.starts_with(b"PK\x03\x04") {
        // We can't tell ZIP vs OOXML vs ODF from magic alone — the
        // discriminator is the [Content_Types].xml mimetype inside.
        // The dispatcher will refine via extension for OOXML/ODF.
        return Some(Kind::Zip);
    }
    // ── gzip: 1F 8B ─────────────────────────────────────────────────
    if head.starts_with(b"\x1f\x8b") {
        // Could be .gz, .tar.gz, .svgz. Extension picks the lane.
        return Some(Kind::TarGz);
    }
    // ── bzip2: BZh ──────────────────────────────────────────────────
    if head.starts_with(b"BZh") {
        return Some(Kind::TarBz2);
    }
    // ── xz: FD 37 7A 58 5A 00 ───────────────────────────────────────
    if head.starts_with(b"\xfd7zXZ\x00") {
        return Some(Kind::TarXz);
    }
    // ── zstd: 28 B5 2F FD ───────────────────────────────────────────
    if head.starts_with(b"\x28\xb5\x2f\xfd") {
        return Some(Kind::TarZst);
    }
    // ── 7z: 37 7A BC AF 27 1C ───────────────────────────────────────
    if head.starts_with(b"7z\xbc\xaf\x27\x1c") {
        return Some(Kind::SevenZ);
    }
    // ── RAR: Rar!\x1a\x07 ──────────────────────────────────────────
    if head.starts_with(b"Rar!\x1a\x07") {
        return Some(Kind::Rar);
    }
    // ── OLE2 compound document (legacy Office .xls/.doc/.ppt) ───────
    //    D0 CF 11 E0 A1 B1 1A E1
    if head.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1") {
        // Extension picks xls vs doc vs ppt — the OLE2 header is the
        // same for all three.
        return None; // discriminate via extension below
    }
    // ── tar: "ustar" at offset 257 ──────────────────────────────────
    if head.len() >= 263 && &head[257..263] == b"ustar\0" {
        return Some(Kind::TarPlain);
    }
    if head.len() >= 265 && &head[257..262] == b"ustar" && &head[262..265] == b"  \0" {
        return Some(Kind::TarPlain);
    }
    None
}

/// Extension-based fallback. Always returns SOMETHING — `Kind::Unknown`
/// for truly unrecognised extensions, which the dispatcher will then
/// route through the text handler with a leading note.
fn sniff_by_extension(path: &Path) -> Kind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        // Source / text
        "txt" | "log" | "ini" | "conf" | "cfg" | "toml" | "yaml" | "yml" | "rs" | "py" | "js"
        | "ts" | "tsx" | "jsx" | "go" | "java" | "c" | "h" | "cpp" | "hpp" | "cc" | "cs"
        | "swift" | "kt" | "scala" | "rb" | "php" | "lua" | "sh" | "bash" | "zsh" | "fish"
        | "ps1" | "sql" | "dockerfile" => Kind::Text,
        // Structured text
        "csv" => Kind::Csv,
        "tsv" => Kind::Tsv,
        "json" => Kind::Json,
        "jsonl" | "ndjson" => Kind::Jsonl,
        "xml" => Kind::Xml,
        "html" | "htm" => Kind::Html,
        "md" | "markdown" => Kind::Markdown,
        "ipynb" => Kind::Notebook,
        "svg" => Kind::Svg,
        // Image
        "png" => Kind::ImagePng,
        "jpg" | "jpeg" => Kind::ImageJpeg,
        "gif" => Kind::ImageGif,
        "webp" => Kind::ImageWebp,
        "bmp" => Kind::ImageBmp,
        "ico" => Kind::ImageIco,
        // PDF
        "pdf" => Kind::Pdf,
        // OOXML (the magic bytes only say ZIP — extension picks the OOXML lane)
        "xlsx" | "xlsm" => Kind::Xlsx,
        "docx" => Kind::Docx,
        "pptx" => Kind::Pptx,
        // ODF
        "ods" => Kind::Ods,
        "odt" => Kind::Odt,
        "odp" => Kind::Odp,
        // Legacy binary Office (OLE2 magic alone can't discriminate)
        "xls" => Kind::LegacyXls,
        "doc" => Kind::LegacyDoc,
        "ppt" => Kind::LegacyPpt,
        // Archives
        "zip" | "jar" | "war" | "ear" | "apk" | "ipa" | "epub" => Kind::Zip,
        "tar" => Kind::TarPlain,
        "gz" | "tgz" => Kind::TarGz,
        "bz2" | "tbz" | "tbz2" => Kind::TarBz2,
        "xz" | "txz" => Kind::TarXz,
        "zst" | "tzst" => Kind::TarZst,
        "7z" => Kind::SevenZ,
        "rar" => Kind::Rar,
        // Nothing matched — let the text handler attempt a lossy decode.
        _ => Kind::Unknown,
    }
}

/// Refine the result of `sniff` when magic alone said ZIP but the
/// extension is OOXML/ODF. Used by the dispatcher after the initial
/// sniff so the model gets the right handler for `.xlsx` instead of
/// just "[ZIP archive]".
pub fn refine_zip(path: &Path, magic_kind: Kind) -> Kind {
    if magic_kind != Kind::Zip {
        return magic_kind;
    }
    let ext_kind = sniff_by_extension(path);
    match ext_kind {
        Kind::Xlsx | Kind::Docx | Kind::Pptx | Kind::Ods | Kind::Odt | Kind::Odp => ext_kind,
        _ => magic_kind,
    }
}

/// Refine TAR.GZ — gzip magic could be a bare `.gz` file or a
/// `.tar.gz`. Extension picks.
pub fn refine_gzip(path: &Path) -> Kind {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Kind::TarGz
    } else if name.ends_with(".svgz") {
        Kind::Svg
    } else {
        Kind::TarGz // best-effort default
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(name: &str) -> PathBuf {
        PathBuf::from(format!("/x/{}", name))
    }

    #[test]
    fn pdf_magic_wins_over_txt_extension() {
        let head = b"%PDF-1.4\n";
        assert_eq!(sniff(&p("not_really.txt"), head), Kind::Pdf);
    }

    #[test]
    fn png_magic_detected() {
        let head = b"\x89PNG\r\n\x1a\nIHDR";
        assert_eq!(sniff(&p("img.png"), head), Kind::ImagePng);
    }

    #[test]
    fn jpeg_magic_detected() {
        let head = b"\xff\xd8\xff\xe0\x00\x10JFIF";
        assert_eq!(sniff(&p("photo.jpg"), head), Kind::ImageJpeg);
    }

    #[test]
    fn webp_requires_riff_and_webp_at_offset_8() {
        let head = b"RIFF\x00\x00\x00\x00WEBPVP8L";
        assert_eq!(sniff(&p("x.webp"), head), Kind::ImageWebp);
    }

    #[test]
    fn zip_magic_returns_zip_by_default() {
        let head = b"PK\x03\x04\x14\x00";
        assert_eq!(sniff(&p("archive.zip"), head), Kind::Zip);
    }

    #[test]
    fn refine_zip_promotes_to_xlsx_by_extension() {
        let head = b"PK\x03\x04";
        let magic = sniff(&p("book.xlsx"), head);
        assert_eq!(magic, Kind::Zip);
        assert_eq!(refine_zip(&p("book.xlsx"), magic), Kind::Xlsx);
    }

    #[test]
    fn refine_zip_promotes_to_odt() {
        let magic = Kind::Zip;
        assert_eq!(refine_zip(&p("doc.odt"), magic), Kind::Odt);
    }

    #[test]
    fn refine_zip_keeps_zip_for_unrelated_extension() {
        let magic = Kind::Zip;
        assert_eq!(refine_zip(&p("blob.zip"), magic), Kind::Zip);
    }

    #[test]
    fn ole2_magic_falls_through_to_extension() {
        let head = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1\x00\x00\x00\x00";
        // OLE2 alone can't tell xls / doc / ppt — extension picks.
        assert_eq!(sniff(&p("book.xls"), head), Kind::LegacyXls);
        assert_eq!(sniff(&p("memo.doc"), head), Kind::LegacyDoc);
        assert_eq!(sniff(&p("deck.ppt"), head), Kind::LegacyPpt);
    }

    #[test]
    fn extension_fallback_for_rust_source() {
        assert_eq!(sniff(&p("main.rs"), b"fn main()"), Kind::Text);
    }

    #[test]
    fn unknown_falls_back_to_unknown_kind() {
        assert_eq!(sniff(&p("blob.xyz"), &[0u8; 32]), Kind::Unknown);
    }

    #[test]
    fn tar_ustar_at_offset_257() {
        let mut head = vec![0u8; 263];
        head[257..263].copy_from_slice(b"ustar\0");
        assert_eq!(sniff(&p("a.tar"), &head), Kind::TarPlain);
    }

    #[test]
    fn media_type_round_trip() {
        assert_eq!(Kind::ImagePng.media_type(), Some("image/png"));
        assert_eq!(Kind::Pdf.media_type(), Some("application/pdf"));
        assert_eq!(Kind::Text.media_type(), None);
    }

    #[test]
    fn carries_visual_payload_only_for_image_pdf() {
        assert!(Kind::ImagePng.carries_visual_payload());
        assert!(Kind::Pdf.carries_visual_payload());
        assert!(!Kind::Svg.carries_visual_payload(), "SVG routes to text");
        assert!(!Kind::Xlsx.carries_visual_payload());
        assert!(!Kind::Text.carries_visual_payload());
    }
}
