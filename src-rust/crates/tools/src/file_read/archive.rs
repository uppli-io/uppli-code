// Archive manifest handler — produces a textual listing for ZIP /
// TAR / TAR.GZ archives. NEVER extracts to disk.
//
// Hardening (every guard is non-negotiable):
//
//   - Pre-flight archive size cap (MAX_ARCHIVE_COMPRESSED).
//   - Total decompressed-bytes cap (MAX_ARCHIVE_DECOMPRESSED).
//   - Per-member compression-ratio guard against zip bombs.
//   - Refuse encrypted ZIP entries (we won't prompt for passwords).
//   - Refuse TAR symlinks / hardlinks / paths containing `..` / paths
//     starting with `/`.
//   - Cap on listed members (MAX_ARCHIVE_MEMBERS).
//
// Output is text-only (blocks = None).
//
// Format coverage:
//   - ZIP / JAR / WAR / EAR / APK / IPA / EPUB → full manifest (they're
//     all ZIPs).
//   - TAR / TAR.GZ / TGZ → full manifest via tar crate over flate2.
//   - TAR.BZ2 / TAR.XZ / TAR.ZST / 7Z / RAR → graceful stub with a
//     Bash recipe. Adding bz2/xz/zstd parsers would require C deps
//     (bzip2 / xz2 / zstd-sys) we intentionally avoid in PR B.

#![allow(dead_code)]

use std::io::{Cursor, Read};
use std::path::Path;

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use tokio::fs;
use zip::ZipArchive;

use super::caption;
use super::detect::Kind;
use super::limits::{
    human_bytes, MAX_ARCHIVE_COMPRESSED, MAX_ARCHIVE_DECOMPRESSED, MAX_COMPRESSION_RATIO,
};
use super::output::HandlerOutput;

/// Bytes used for the format-sniff column.
///
/// Hardcoded: 4 KiB is sufficient for all known magic-byte signatures;
/// raising it costs RAM per entry with zero detection upside.
const SNIFF_BYTES: usize = 4 * 1024;

pub async fn read_archive(path: &Path, kind: Kind, cfg: &cc_core::config::Config) -> HandlerOutput {
    let max_archive_members = cfg.effective_max_archive_members();
    let display = path.display().to_string();

    let meta = match fs::symlink_metadata(path).await {
        Ok(m) => m,
        Err(e) => {
            return HandlerOutput::error_text(format!(
                "[Archive read failed for {}: {}]",
                display, e
            ))
        }
    };
    if meta.file_type().is_symlink() {
        return HandlerOutput::error_text(format!(
            "[Archive read refused: {} is a symlink]",
            display
        ));
    }
    if !meta.is_file() {
        return HandlerOutput::error_text(format!(
            "[Archive read refused: {} is not a regular file]",
            display
        ));
    }
    let size = meta.len();
    if size > MAX_ARCHIVE_COMPRESSED {
        return HandlerOutput::error_text(format!(
            "[Archive: {} is {} > {} compressed cap]",
            display,
            human_bytes(size),
            human_bytes(MAX_ARCHIVE_COMPRESSED)
        ));
    }

    match kind {
        Kind::Zip => render_zip(path, size, max_archive_members).await,
        Kind::TarPlain => render_tar(path, size, false, max_archive_members).await,
        Kind::TarGz => render_tar(path, size, true, max_archive_members).await,
        Kind::TarBz2 => stub(path, "TAR.BZ2", "tar -xjf"),
        Kind::TarXz => stub(path, "TAR.XZ", "tar -xJf"),
        Kind::TarZst => stub(path, "TAR.ZST", "tar --zstd -xf"),
        Kind::SevenZ => stub(path, "7Z", "7z x"),
        Kind::Rar => stub(path, "RAR", "unar"),
        other => HandlerOutput::error_text(format!(
            "[Archive handler called for non-archive kind: {:?}]",
            other
        )),
    }
}

fn stub(path: &Path, kind_label: &str, recipe_cmd: &str) -> HandlerOutput {
    let recipe = format!("Use Bash: `{} \"{}\"`.", recipe_cmd, path.display());
    let cap = caption::stub(path, kind_label, &recipe);
    HandlerOutput::success_text(cap)
}

// ── ZIP ──────────────────────────────────────────────────────────────────

async fn render_zip(path: &Path, size: u64, max_archive_members: usize) -> HandlerOutput {
    let display = path.display().to_string();
    let bytes = match fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            return HandlerOutput::error_text(format!(
                "[Archive read failed for {}: {}]",
                display, e
            ))
        }
    };
    let cursor = Cursor::new(bytes);
    let mut archive = match ZipArchive::new(cursor) {
        Ok(a) => a,
        Err(e) => {
            return HandlerOutput::error_text(format!("[ZIP: {} not a valid zip: {}]", display, e))
        }
    };

    let total = archive.len();
    let mut manifest = String::new();
    let mut listed = 0usize;
    let mut skipped_encrypted = 0usize;
    let mut skipped_traversal = 0usize;
    let mut cumulative_decompressed: u64 = 0;
    let mut bomb_aborted = false;

    for i in 0..total {
        if listed >= max_archive_members {
            break;
        }
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.encrypted() {
            skipped_encrypted += 1;
            continue;
        }
        // enclosed_name returns None when the entry name contains
        // traversal components (`..`), is absolute, or would escape the
        // logical root. Exactly the guard we want.
        let name = match entry.enclosed_name() {
            Some(n) => n,
            None => {
                skipped_traversal += 1;
                continue;
            }
        };
        if entry.is_dir() {
            continue;
        }
        let entry_size = entry.size();
        let compressed = entry.compressed_size();

        // Per-member ratio guard. A 1-byte entry that claims to
        // decompress to 200 bytes is fine; 1 KiB → 200 MiB is a bomb.
        if compressed > 0 && entry_size / compressed.max(1) > MAX_COMPRESSION_RATIO {
            bomb_aborted = true;
            break;
        }
        // Global decompressed cap.
        let next = cumulative_decompressed.saturating_add(entry_size);
        if next > MAX_ARCHIVE_DECOMPRESSED {
            bomb_aborted = true;
            break;
        }
        cumulative_decompressed = next;

        // Read up to 4 KiB for the sniff + fingerprint.
        let mut head = vec![0u8; SNIFF_BYTES];
        let read_n = read_to_fill(&mut entry, &mut head);
        head.truncate(read_n);
        let sha8 = sha256_first_8(&head);
        let inferred = super::detect::sniff(name.as_path(), &head).label();

        manifest.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            name.display(),
            entry_size,
            sha8,
            inferred
        ));
        listed += 1;
    }

    let mut out = caption::archive(path, "ZIP", listed, size);
    out.push_str("\n\n");
    out.push_str(&manifest);
    if skipped_encrypted > 0 {
        out.push_str(&format!(
            "\n[Note: skipped {} encrypted entries — pass the password out-of-band.]\n",
            skipped_encrypted
        ));
    }
    if skipped_traversal > 0 {
        out.push_str(&format!(
            "\n[Note: skipped {} entries with path-traversal segments (..) — likely malicious.]\n",
            skipped_traversal
        ));
    }
    if total > listed {
        out.push_str(&format!(
            "\n[Truncated: listed {} of {} members. Use Bash: `unzip -l \"{}\"` for the full listing.]\n",
            listed,
            total,
            path.display()
        ));
    }
    if bomb_aborted {
        out.push_str("\n[Aborted: archive failed compression-ratio or cumulative-decompressed bomb guard.]\n");
    }
    HandlerOutput::success_text(out)
}

// ── TAR + TAR.GZ ─────────────────────────────────────────────────────────

async fn render_tar(path: &Path, size: u64, gz: bool, max_archive_members: usize) -> HandlerOutput {
    let display = path.display().to_string();
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return HandlerOutput::error_text(format!("[TAR read failed for {}: {}]", display, e))
        }
    };
    // CRITICAL: take(MAX_ARCHIVE_DECOMPRESSED) wraps the DECOMPRESSED
    // stream for .tar.gz so a gzip bomb can't blow past the cap. For
    // plain .tar the underlying file is already pre-flight-capped at
    // MAX_ARCHIVE_COMPRESSED.
    let inner: Box<dyn Read> = if gz {
        Box::new(GzDecoder::new(file).take(MAX_ARCHIVE_DECOMPRESSED + 1))
    } else {
        Box::new(file)
    };
    let mut tar = tar::Archive::new(inner);

    let mut manifest = String::new();
    let mut listed = 0usize;
    let mut skipped_traversal = 0usize;
    let mut skipped_link = 0usize;
    let mut cumulative_decompressed: u64 = 0;
    let mut bomb_aborted = false;

    let entries = match tar.entries() {
        Ok(e) => e,
        Err(e) => {
            return HandlerOutput::error_text(format!(
                "[TAR: {} entries unreadable: {}]",
                display, e
            ))
        }
    };

    for entry in entries {
        if listed >= max_archive_members {
            break;
        }
        let mut entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        // Reject non-regular files (symlinks, hardlinks, devices, etc.)
        let etype = entry.header().entry_type();
        if etype.is_symlink() || etype.is_hard_link() {
            skipped_link += 1;
            continue;
        }
        if !etype.is_file() {
            continue;
        }
        // Path-traversal guard.
        let raw_path = match entry.path() {
            Ok(p) => p.into_owned(),
            Err(_) => continue,
        };
        if raw_path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        }) {
            skipped_traversal += 1;
            continue;
        }
        let entry_size = entry.size();
        let next = cumulative_decompressed.saturating_add(entry_size);
        if next > MAX_ARCHIVE_DECOMPRESSED {
            bomb_aborted = true;
            break;
        }
        cumulative_decompressed = next;

        let mut head = vec![0u8; SNIFF_BYTES];
        let read_n = read_to_fill(&mut entry, &mut head);
        head.truncate(read_n);
        let sha8 = sha256_first_8(&head);
        let inferred = super::detect::sniff(&raw_path, &head).label();

        manifest.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            raw_path.display(),
            entry_size,
            sha8,
            inferred
        ));
        listed += 1;
    }

    let kind_label = if gz { "TAR.GZ" } else { "TAR" };
    let mut out = caption::archive(path, kind_label, listed, size);
    out.push_str("\n\n");
    out.push_str(&manifest);
    if skipped_link > 0 {
        out.push_str(&format!(
            "\n[Note: skipped {} symlink/hardlink entries — refused as a precaution.]\n",
            skipped_link
        ));
    }
    if skipped_traversal > 0 {
        out.push_str(&format!(
            "\n[Note: skipped {} entries with absolute or traversal paths — likely malicious.]\n",
            skipped_traversal
        ));
    }
    if bomb_aborted {
        out.push_str("\n[Aborted: cumulative decompressed size exceeded cap — suspected bomb.]\n");
    }
    HandlerOutput::success_text(out)
}

// ── helpers ──────────────────────────────────────────────────────────────

/// Fill `buf` from `r` by looping until EOF or buf is full. `Read::read`
/// is allowed to return less than requested per call; without this loop
/// we'd get truncated sha256 fingerprints from chunked readers.
fn read_to_fill<R: Read>(r: &mut R, buf: &mut [u8]) -> usize {
    let mut written = 0;
    while written < buf.len() {
        match r.read(&mut buf[written..]) {
            Ok(0) => break,
            Ok(n) => written += n,
            Err(_) => break,
        }
    }
    written
}

fn sha256_first_8(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(16);
    for &b in digest.iter().take(8) {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut writer = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, content) in files {
                writer.start_file(*name, opts).unwrap();
                writer.write_all(content).unwrap();
            }
            writer.finish().unwrap();
        }
        buf
    }

    #[tokio::test]
    async fn zip_manifest_lists_members() {
        let bytes = make_zip(&[("a.txt", b"hello"), ("dir/b.bin", b"\x00\x01\x02")]);
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.zip");
        std::fs::write(&path, &bytes).unwrap();
        let cfg = cc_core::config::Config::default();
        let out = read_archive(&path, Kind::Zip, &cfg).await;
        assert!(!out.is_error, "expected success, got: {}", out.content);
        assert!(out.content.contains("a.txt"));
        assert!(out.content.contains("dir/b.bin"));
        // sha256_first_8 column present
        assert!(out.content.contains('\t'));
    }

    #[tokio::test]
    async fn tar_bz2_returns_stub_with_recipe() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.tar.bz2");
        std::fs::write(&path, b"BZh placeholder").unwrap();
        let cfg = cc_core::config::Config::default();
        let out = read_archive(&path, Kind::TarBz2, &cfg).await;
        assert!(!out.is_error);
        assert!(out.content.contains("tar -xjf"));
    }

    #[test]
    fn sha256_first_8_is_16_hex_chars() {
        let s = sha256_first_8(b"hello");
        assert_eq!(s.len(), 16);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
