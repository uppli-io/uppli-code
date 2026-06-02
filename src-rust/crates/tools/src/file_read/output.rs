#![allow(dead_code)]
// `dead_code` is silenced for the module because handlers that consume
// `HandlerOutput` land in subsequent commits of PR B. Removed once the
// last handler is wired up (commit 13 or earlier).

// HandlerOutput — the contract every format handler returns.
//
// Centralises the tool-level packaging decision: when does the tool
// emit `ToolResult.blocks` vs fold everything into `content`?
//
// Since the CLI became provider-agnostic (PR C), the query loop always
// forwards `ToolResult.blocks` verbatim to the provider when present.
// It is the PROVIDER's translation layer that degrades visual blocks
// for non-vision models (see `AnthropicClient::degrade_blocks_if_needed`
// and `OpenAiProvider::translate_message`). So this struct's job is:
//
//   - `ToolResult.content` is ALWAYS a non-empty self-sufficient text
//     string. The provider may strip Image/Document blocks before they
//     reach the model, leaving `content` as the only signal — it must
//     stand alone.
//   - `ToolResult.blocks` is `Some(v)` iff v is non-empty AND contains
//     at least one Image or Document block. Pure text-only blocks are
//     folded into `content` here because no provider currently needs
//     them as structured payloads.

use crate::ToolResult;
use cc_core::types::ContentBlock;
use std::path::Path;

// Runtime cap on `ContentBlock`s carried by a single FileRead tool
// result flows through `Config::effective_max_blocks_per_result`
// (knob: --max-blocks-per-result); the fallback constant lives in
// `cc_core::constants::DEFAULT_MAX_BLOCKS_PER_RESULT` and is imported
// inside `mod tests` where it is exercised directly.

/// Description of a truncation that happened inside a handler. Used by
/// `finalize` to append a canonical footer to `content` instead of every
/// handler hand-rolling its own message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Truncation {
    /// Text path truncated mid-stream because MAX_TEXT_BYTES was reached.
    Bytes { shown: u64, total: u64 },
    /// Text/CSV path truncated by line offset/limit.
    Lines { shown: usize, total: usize },
    /// PDF/PPTX page count exceeded MAX_*_PAGES.
    Pages { shown: usize, total: usize },
    /// Archive listing truncated at MAX_ARCHIVE_MEMBERS.
    Members { shown: usize, total: usize },
}

impl Truncation {
    fn footer(&self) -> String {
        match self {
            Truncation::Bytes { shown, total } => format!(
                "\n... ({} more bytes elided, {} bytes total. Use Bash + head/tail/sed for the rest.)\n",
                total.saturating_sub(*shown),
                total
            ),
            Truncation::Lines { shown, total } => format!(
                "\n... ({} more lines, {} total. Use offset/limit to read more.)\n",
                total.saturating_sub(*shown),
                total
            ),
            Truncation::Pages { shown, total } => format!(
                "\n... ({} more pages, {} total. Use the `pages` parameter to read specific ranges.)\n",
                total.saturating_sub(*shown),
                total
            ),
            Truncation::Members { shown, total } => format!(
                "\n... ({} more members, {} total. Use Bash with unzip / tar -t for the full listing.)\n",
                total.saturating_sub(*shown),
                total
            ),
        }
    }
}

/// Output produced by a single format handler. Always carries a textual
/// fallback; `blocks` is optional and only structured payloads belong
/// there. `finalize` is the only entry point that converts this into a
/// `ToolResult`.
#[derive(Debug, Default, Clone)]
pub struct HandlerOutput {
    /// Textual fallback. ALWAYS populated by the handler before calling
    /// `finalize` — that is the contract. Empty content panics in debug
    /// builds and produces a synthetic placeholder in release builds.
    pub content: String,
    /// Optional structured payload (image, document, multi-part text).
    /// Always empty when the handler has no binary content to attach.
    pub blocks: Vec<ContentBlock>,
    /// Truncation footer to append to `content`.
    pub truncation: Option<Truncation>,
    /// If true, `finalize` produces `ToolResult::error`. Defaults to
    /// success — handlers opt in to error only for hard failures (file
    /// > MAX_FILE_BYTES, bomb detected, etc.).
    pub is_error: bool,
}

impl HandlerOutput {
    /// Convenience constructor: a successful text-only result.
    pub fn success_text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            blocks: Vec::new(),
            truncation: None,
            is_error: false,
        }
    }

    /// Convenience constructor: a hard error.
    pub fn error_text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            blocks: Vec::new(),
            truncation: None,
            is_error: true,
        }
    }

    /// Convert into the final `ToolResult`. Single dispatch point for the
    /// blocks-vs-text decision. Enforces every invariant the per-handler
    /// commits rely on.
    pub fn finalize(mut self, path: &Path, max_blocks: usize) -> ToolResult {
        // Footer first so the caption survives untruncated above it.
        if let Some(t) = self.truncation.as_ref() {
            self.content.push_str(&t.footer());
        }

        // Invariant 1: content always non-empty. Per the contract every
        // handler populates it, but a bug in a future handler shouldn't
        // emit an empty tool_result (the budget guard at query/src/lib.rs
        // would let it balloon silently).
        if self.content.is_empty() {
            debug_assert!(
                false,
                "HandlerOutput.content must be non-empty before finalize"
            );
            self.content = format!("[Read {} — empty handler output]", path.display());
        }

        // Invariant 2: blocks length capped at DEFAULT_MAX_BLOCKS_PER_RESULT.
        // Defensive guard against a handler that streams many small
        // sub-images and forgets its own cap. The budget guard doesn't
        // see Image/Document payloads (TODO(pr-c)), so this is the only
        // thing keeping a runaway handler bounded.
        if self.blocks.len() > max_blocks {
            self.blocks.truncate(max_blocks);
            self.content.push_str(&format!(
                "\n[Note: block list truncated to {} entries — handler emitted more.]\n",
                max_blocks
            ));
        }

        // Invariant 3: only emit blocks when they actually carry an Image
        // or Document. Pure-text structured blocks (a future multi-part
        // table dialect, etc.) are valuable on capable providers but
        // we don't have an emitter for them yet — fold them into
        // content silently. The provider's translation layer is the
        // one that decides whether to forward Image/Document to the
        // model or degrade to text (see AnthropicClient::
        // degrade_blocks_if_needed and OpenAiProvider::translate_message).
        let has_visual = self.blocks.iter().any(|b| {
            matches!(
                b,
                ContentBlock::Image { .. } | ContentBlock::Document { .. }
            )
        });

        if self.is_error {
            ToolResult::error(self.content)
        } else if has_visual && !self.blocks.is_empty() {
            ToolResult::success_with_blocks(self.content, self.blocks)
        } else {
            ToolResult::success(self.content)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::constants::DEFAULT_MAX_BLOCKS_PER_RESULT;
    use cc_core::types::ImageSource;
    use std::path::PathBuf;

    fn dummy_path() -> PathBuf {
        PathBuf::from("/tmp/test.bin")
    }

    fn image_block() -> ContentBlock {
        ContentBlock::Image {
            source: ImageSource {
                source_type: "base64".to_string(),
                media_type: Some("image/png".to_string()),
                data: Some("iVBORw0KGgo=".to_string()),
                url: None,
            },
        }
    }

    #[test]
    fn finalize_text_only_produces_success_no_blocks() {
        let out = HandlerOutput::success_text("hello world");
        let r = out.finalize(&dummy_path(), DEFAULT_MAX_BLOCKS_PER_RESULT);
        assert!(!r.is_error);
        assert_eq!(r.content, "hello world");
        assert!(r.blocks.is_none(), "text-only must not populate blocks");
    }

    #[test]
    fn finalize_visual_blocks_produces_success_with_blocks() {
        let out = HandlerOutput {
            content: "[Image: dummy 1x1 png]".to_string(),
            blocks: vec![image_block()],
            truncation: None,
            is_error: false,
        };
        let r = out.finalize(&dummy_path(), DEFAULT_MAX_BLOCKS_PER_RESULT);
        assert!(!r.is_error);
        assert_eq!(r.content, "[Image: dummy 1x1 png]");
        let blocks = r.blocks.expect("blocks must be Some");
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn finalize_error_path_drops_blocks() {
        // Even if the handler set blocks, an error result is text-only —
        // the model interprets is_error=true as "the payload is unusable".
        let out = HandlerOutput {
            content: "boom".to_string(),
            blocks: vec![image_block()],
            truncation: None,
            is_error: true,
        };
        let r = out.finalize(&dummy_path(), DEFAULT_MAX_BLOCKS_PER_RESULT);
        assert!(r.is_error);
        assert!(r.blocks.is_none(), "error result must not carry blocks");
    }

    #[test]
    fn finalize_appends_truncation_footer() {
        let out = HandlerOutput {
            content: "row 1\nrow 2\n".to_string(),
            blocks: Vec::new(),
            truncation: Some(Truncation::Lines {
                shown: 2,
                total: 100,
            }),
            is_error: false,
        };
        let r = out.finalize(&dummy_path(), DEFAULT_MAX_BLOCKS_PER_RESULT);
        assert!(r.content.contains("row 1"));
        assert!(r.content.contains("98 more lines"));
        assert!(r.content.contains("100 total"));
    }

    #[test]
    fn finalize_caps_blocks_at_max() {
        let blocks: Vec<ContentBlock> = (0..DEFAULT_MAX_BLOCKS_PER_RESULT + 5)
            .map(|_| image_block())
            .collect();
        let out = HandlerOutput {
            content: "many images".to_string(),
            blocks,
            truncation: None,
            is_error: false,
        };
        let r = out.finalize(&dummy_path(), DEFAULT_MAX_BLOCKS_PER_RESULT);
        let blocks = r.blocks.expect("blocks must be Some");
        assert_eq!(blocks.len(), DEFAULT_MAX_BLOCKS_PER_RESULT);
        assert!(
            r.content.contains("truncated"),
            "must note the cap was hit, got: {}",
            r.content
        );
    }

    #[test]
    fn finalize_text_only_blocks_get_folded_into_content() {
        // A handler that emits only Text blocks (no Image/Document) MUST
        // produce a text-only ToolResult. Pure-text blocks have no live
        // emitter in PR B and the dispatch criterion is "carries visual
        // payload" — keep it that way.
        let out = HandlerOutput {
            content: "table content".to_string(),
            blocks: vec![ContentBlock::Text {
                text: "row".to_string(),
            }],
            truncation: None,
            is_error: false,
        };
        let r = out.finalize(&dummy_path(), DEFAULT_MAX_BLOCKS_PER_RESULT);
        assert!(
            r.blocks.is_none(),
            "text-only blocks must NOT promote to ToolResult.blocks"
        );
    }
}
