// Image format handler.
//
// Pipeline (intentionally minimal in PR B — resize/re-encode deferred
// to PR C once telemetry tells us how often the pixel cap fires):
//
//   1. fs::metadata + size cap (MAX_IMAGE_BYTES). Over cap → caption
//      only, blocks=None.
//   2. Open via `fs::File::open` (we deliberately do NOT chase
//      symlinks blindly; the dispatcher in mod.rs only allows reads
//      under the ToolContext's working_dir via `resolve_path`, but
//      we add a defense-in-depth symlink_metadata check anyway).
//   3. Read full bytes with a hard cap (TOCTOU defense: a swap
//      between metadata and read could enlarge the file).
//   4. Probe dimensions in `spawn_blocking` via
//      `image::ImageReader::new(Cursor).with_guessed_format()`
//      WITHOUT decoding pixels — defeats decompression bombs.
//   5. If pixels > MAX_IMAGE_PIXELS → caption only (no decode).
//   6. Otherwise emit ContentBlock::Image with the raw bytes
//      base64-encoded and the sniffed media_type.
//
// SVG → delegated to the text handler (it IS XML).
// ICO → caption-only stub (icons are noise for vision models).
//
// What this handler intentionally does NOT do (deferred to PR C):
//   - No JPEG re-encoding (avoids EXIF orientation loss + chroma
//     subsampling corruption flagged by reviewer #8).
//   - No resize/downscale loop (pixel cap is a refusal, not a
//     transformation — keeps the handler panic-free and bounded).
//   - No HEIC / AVIF support (image crate's HEIC decoder is
//     experimental).
//   - No re-encode of GIF → first frame only (gifs would need a
//     separate path).

#![allow(dead_code)]

use std::io::Cursor;
use std::path::Path;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use cc_core::types::{ContentBlock, ImageSource};
use image::{ImageFormat, ImageReader};
use tokio::fs;

use super::caption;
use super::detect::Kind;
use super::limits::{MAX_IMAGE_BYTES, MAX_IMAGE_PIXELS};
use super::output::HandlerOutput;

/// Read an image file and produce a `HandlerOutput` carrying a
/// `ContentBlock::Image` when the file passes the size + pixel caps,
/// or a caption-only fallback otherwise.
///
/// `kind` is passed by the dispatcher so we route SVG → text and
/// ICO → stub without re-sniffing.
pub async fn read_image(path: &Path, kind: Kind) -> HandlerOutput {
    let display = path.display().to_string();

    // ── SVG → text handler (it is XML) ───────────────────────────────
    if matches!(kind, Kind::Svg) {
        return super::text::read_text(path, None, None).await;
    }

    // ── ICO → stub. Icons are typically multi-frame 16×16/32×32
    //    containers and signal nothing useful to vision models.
    if matches!(kind, Kind::ImageIco) {
        return HandlerOutput::success_text(format!(
            "[ICO file: {}, not forwarded as image.]",
            display
        ));
    }

    // ── 1. metadata + size cap ───────────────────────────────────────
    let meta = match fs::symlink_metadata(path).await {
        Ok(m) => m,
        Err(e) => {
            return HandlerOutput::error_text(format!(
                "[Image read failed for {}: {}]",
                display, e
            ));
        }
    };
    if meta.file_type().is_symlink() {
        return HandlerOutput::error_text(format!(
            "[Image read refused: {} is a symlink; resolve and pass the real path]",
            display
        ));
    }
    if !meta.is_file() {
        return HandlerOutput::error_text(format!(
            "[Image read refused: {} is not a regular file]",
            display
        ));
    }
    let size = meta.len();
    if size == 0 {
        return HandlerOutput::success_text(format!(
            "[Image: {}, empty file (0 bytes), not forwarded]",
            display
        ));
    }
    if size > MAX_IMAGE_BYTES {
        return HandlerOutput::success_text(format!(
            "[Image: {}, {} exceeds inline cap {}, not forwarded as image block]",
            display,
            super::limits::human_bytes(size),
            super::limits::human_bytes(MAX_IMAGE_BYTES),
        ));
    }

    // ── 2. read bytes (with TOCTOU growth-check) ─────────────────────
    let bytes = match fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            return HandlerOutput::error_text(format!(
                "[Image read failed for {}: {}]",
                display, e
            ));
        }
    };
    if (bytes.len() as u64) > MAX_IMAGE_BYTES {
        return HandlerOutput::success_text(format!(
            "[Image: {}, grew past size cap mid-read ({}), not forwarded]",
            display,
            super::limits::human_bytes(bytes.len() as u64),
        ));
    }

    // ── 3. probe dimensions WITHOUT decoding pixels ──────────────────
    //
    // ImageReader::with_guessed_format reads just enough of the header
    // to identify format + dimensions. `into_dimensions` does NOT decode
    // pixels — it parses the IHDR / SOFn / VP8 header. Decompression
    // bombs (a 10 KiB PNG that claims to be 100k×100k) are caught here.
    //
    // Run in spawn_blocking even though the work is small — `image`
    // crate methods are sync and may parse adversarial headers slowly.
    let probe_bytes = bytes.clone();
    let probe =
        tokio::task::spawn_blocking(move || -> Result<(u32, u32, Option<ImageFormat>), String> {
            let cursor = Cursor::new(&probe_bytes);
            let reader = ImageReader::new(cursor)
                .with_guessed_format()
                .map_err(|e| format!("guess format: {}", e))?;
            let fmt = reader.format();
            let dims = reader
                .into_dimensions()
                .map_err(|e| format!("read dimensions: {}", e))?;
            Ok((dims.0, dims.1, fmt))
        })
        .await;

    let (width, height, fmt) = match probe {
        Err(join_err) => {
            return HandlerOutput::error_text(format!(
                "[Image probe panicked for {}: {}]",
                display, join_err
            ));
        }
        Ok(Err(e)) => {
            return HandlerOutput::error_text(format!(
                "[Image probe failed for {}: {}]",
                display, e
            ));
        }
        Ok(Ok(v)) => v,
    };

    if width == 0 || height == 0 {
        return HandlerOutput::error_text(format!(
            "[Image: {} reports zero-dimension ({}x{}), refusing]",
            display, width, height
        ));
    }

    // ── 4. pixel-count cap (decompression bomb refusal) ──────────────
    let pixels = (width as u64).saturating_mul(height as u64);
    if pixels > MAX_IMAGE_PIXELS {
        return HandlerOutput::success_text(format!(
            "[Image: {}, {}x{} ({} pixels) exceeds pixel cap {}, not forwarded]",
            display, width, height, pixels, MAX_IMAGE_PIXELS,
        ));
    }

    // ── 5. resolve media_type ────────────────────────────────────────
    //
    // Prefer the probe-derived format; fall back to the dispatcher's
    // Kind when the probe couldn't determine it (rare — typically
    // happens on edge formats).
    let media_type = match fmt {
        Some(ImageFormat::Png) => "image/png",
        Some(ImageFormat::Jpeg) => "image/jpeg",
        Some(ImageFormat::Gif) => "image/gif",
        Some(ImageFormat::WebP) => "image/webp",
        Some(ImageFormat::Bmp) => "image/bmp",
        Some(_) | None => match kind.media_type() {
            Some(mt) => mt,
            None => {
                return HandlerOutput::success_text(format!(
                    "[Image: {}, {}x{}, unsupported format, not forwarded]",
                    display, width, height
                ));
            }
        },
    };

    // ── 6. emit ContentBlock::Image + caption ────────────────────────
    let data = B64.encode(&bytes);
    let block = ContentBlock::Image {
        source: ImageSource {
            source_type: "base64".to_string(),
            media_type: Some(media_type.to_string()),
            data: Some(data),
            url: None,
        },
    };
    let cap = caption::image(path, width, height, media_type, size);

    HandlerOutput {
        content: cap,
        blocks: vec![block],
        truncation: None,
        is_error: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    // 1×1 transparent PNG (smallest valid PNG, 67 bytes).
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

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
    async fn tiny_png_produces_image_block() {
        let (_dir, path) = write_tmp(TINY_PNG, "tiny.png");
        let out = read_image(&path, Kind::ImagePng).await;
        assert!(!out.is_error, "tiny PNG should succeed: {}", out.content);
        assert_eq!(out.blocks.len(), 1);
        match &out.blocks[0] {
            ContentBlock::Image { source } => {
                assert_eq!(source.source_type, "base64");
                assert_eq!(source.media_type.as_deref(), Some("image/png"));
                assert!(source.data.as_ref().unwrap().starts_with("iVBORw"));
            }
            other => panic!("expected Image block, got {:?}", other),
        }
        // Caption mentions dimensions
        assert!(
            out.content.contains("1x1"),
            "caption missing WxH: {}",
            out.content
        );
        assert!(out.content.contains("image/png"));
    }

    #[tokio::test]
    async fn svg_delegates_to_text_handler() {
        let svg = b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"10\"/></svg>";
        let (_dir, path) = write_tmp(svg, "icon.svg");
        let out = read_image(&path, Kind::Svg).await;
        assert!(!out.is_error);
        assert!(out.blocks.is_empty(), "SVG must not emit an Image block");
        assert!(out.content.contains("<svg"));
    }

    #[tokio::test]
    async fn ico_stub_is_caption_only() {
        let (_dir, path) = write_tmp(b"fake ico content", "favicon.ico");
        let out = read_image(&path, Kind::ImageIco).await;
        assert!(!out.is_error);
        assert!(out.blocks.is_empty());
        assert!(out.content.contains("ICO"));
    }

    #[tokio::test]
    async fn empty_file_returns_placeholder() {
        let (_dir, path) = write_tmp(&[], "empty.png");
        let out = read_image(&path, Kind::ImagePng).await;
        assert!(!out.is_error);
        assert!(out.blocks.is_empty());
        assert!(out.content.contains("empty file"));
    }

    #[tokio::test]
    async fn corrupted_image_returns_probe_error() {
        // Has PNG magic but truncated IHDR → probe fails.
        let (_dir, path) = write_tmp(b"\x89PNG\r\n\x1a\n", "broken.png");
        let out = read_image(&path, Kind::ImagePng).await;
        assert!(out.is_error, "truncated PNG should error");
        assert!(
            out.content.contains("probe failed") || out.content.contains("probe panicked"),
            "got: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn size_cap_fallback_is_caption_only() {
        // Build a file larger than MAX_IMAGE_BYTES.
        let huge = vec![0u8; (MAX_IMAGE_BYTES + 1) as usize];
        let (_dir, path) = write_tmp(&huge, "huge.png");
        let out = read_image(&path, Kind::ImagePng).await;
        assert!(!out.is_error, "size cap is a soft fallback");
        assert!(out.blocks.is_empty());
        assert!(out.content.contains("exceeds inline cap"));
    }

    #[tokio::test]
    async fn symlink_is_refused() {
        // Skipped on Windows where symlinks need admin rights.
        #[cfg(unix)]
        {
            let (dir, real) = write_tmp(TINY_PNG, "real.png");
            let link = dir.path().join("link.png");
            std::os::unix::fs::symlink(&real, &link).expect("symlink");
            let out = read_image(&link, Kind::ImagePng).await;
            assert!(out.is_error);
            assert!(out.content.contains("symlink"));
        }
    }
}
