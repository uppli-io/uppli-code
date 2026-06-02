// CSV / TSV handler.
//
// PR B treats tabular files as text with a small header banner — no
// RFC-4180 quote parsing yet. The legacy `<num>\t<line>\n` shape is
// preserved so the TUI renderer and CodeAudit hook keep working
// unchanged. The banner gives the model an explicit cue that the
// content is row-oriented.

#![allow(dead_code)]

use std::path::Path;

use super::detect::Kind;
use super::output::HandlerOutput;

pub async fn read_tabular(
    path: &Path,
    kind: Kind,
    offset: Option<usize>,
    limit: Option<usize>,
) -> HandlerOutput {
    let mut out = super::text::read_text(path, offset, limit).await;
    if !out.is_error {
        let banner = match kind {
            Kind::Csv => format!(
                "[CSV: {} — comma-separated; row 1 is likely the header.]\n",
                path.display()
            ),
            Kind::Tsv => format!(
                "[TSV: {} — tab-separated; row 1 is likely the header.]\n",
                path.display()
            ),
            _ => String::new(),
        };
        if !banner.is_empty() {
            out.content = format!("{}{}", banner, out.content);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[tokio::test]
    async fn csv_banner_prepended() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("data.csv");
        std::fs::File::create(&p)
            .unwrap()
            .write_all(b"a,b,c\n1,2,3\n")
            .unwrap();
        let out = read_tabular(&p, Kind::Csv, None, None).await;
        assert!(!out.is_error);
        assert!(out.content.starts_with("[CSV"));
        assert!(out.content.contains("a,b,c"));
    }

    #[tokio::test]
    async fn tsv_banner_prepended() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("data.tsv");
        std::fs::File::create(&p)
            .unwrap()
            .write_all(b"a\tb\tc\n1\t2\t3\n")
            .unwrap();
        let out = read_tabular(&p, Kind::Tsv, None, None).await;
        assert!(!out.is_error);
        assert!(out.content.starts_with("[TSV"));
    }
}
