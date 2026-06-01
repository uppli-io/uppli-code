// Legacy binary Office formats (.xls / .doc / .ppt) — graceful stub.
//
// Pure-Rust readers for the OLE2 compound document family are either
// experimental or behind C bindings. Rather than ship something that
// may panic on hostile .xls files, we return a successful stub with
// a libreoffice recipe so the model can route around the gap.
//
// is_error = FALSE intentionally: the model gets actionable guidance
// instead of a red wall, which matches the user's "tt doit etre
// envoyé" directive (every format reaches the LLM — even if the
// content this time is a recipe rather than text).

#![allow(dead_code)]

use std::path::Path;

use super::caption;
use super::detect::Kind;
use super::output::HandlerOutput;

pub async fn read_legacy_office(path: &Path, kind: Kind) -> HandlerOutput {
    let (kind_label, target_fmt) = match kind {
        Kind::LegacyXls => ("Legacy XLS", "xlsx"),
        Kind::LegacyDoc => ("Legacy DOC", "docx"),
        Kind::LegacyPpt => ("Legacy PPT", "pptx"),
        other => {
            return HandlerOutput::error_text(format!(
                "[Legacy office handler called for non-legacy kind: {:?}]",
                other
            ))
        }
    };
    let recipe = format!(
        "Use Bash: `libreoffice --headless --convert-to {} \"{}\"` then Read the converted file.",
        target_fmt,
        path.display()
    );
    let cap = caption::stub(path, kind_label, &recipe);
    HandlerOutput::success_text(cap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn xls_stub_mentions_libreoffice() {
        let out = read_legacy_office(&PathBuf::from("/tmp/foo.xls"), Kind::LegacyXls).await;
        assert!(!out.is_error);
        assert!(out.content.contains("libreoffice"));
        assert!(out.content.contains("xlsx"));
    }

    #[tokio::test]
    async fn doc_stub_mentions_docx_target() {
        let out = read_legacy_office(&PathBuf::from("/tmp/foo.doc"), Kind::LegacyDoc).await;
        assert!(!out.is_error);
        assert!(out.content.contains("docx"));
    }
}
