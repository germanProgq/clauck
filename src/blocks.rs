use crate::error::{ClauckError, ErrorCode, Result};
use crate::fsutil::sha256_hex;

pub const BEGIN_PREFIX: &str = "<!-- clauck:begin";
pub const BEGIN_LINE: &str =
    "<!-- clauck:begin (managed by Clauck; `clauck uninstall` removes this block) -->";
pub const END_LINE: &str = "<!-- clauck:end -->";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn text<'a>(&self, doc: &'a str) -> &'a str {
        &doc[self.start..self.end]
    }
}

pub fn wrap(body: &str) -> String {
    format!("{BEGIN_LINE}\n{}\n{END_LINE}", body.trim_end())
}

pub fn block_hash(block: &str) -> String {
    sha256_hex(normalize(block).trim_end_matches('\n').as_bytes())
}

fn normalize(s: &str) -> String {
    s.replace("\r\n", "\n")
}

fn uses_crlf(doc: &str) -> bool {
    doc.contains("\r\n")
}

fn with_line_endings(block_lf: &str, crlf: bool) -> String {
    if crlf {
        block_lf.replace('\n', "\r\n")
    } else {
        block_lf.to_string()
    }
}

pub fn find(doc: &str) -> Result<Option<Span>> {
    let mut offset = 0;
    let mut begin: Option<usize> = None;
    let mut found: Option<Span> = None;
    for line in doc.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with(BEGIN_PREFIX) {
            if begin.is_some() || found.is_some() {
                return Err(malformed("more than one clauck:begin marker"));
            }
            begin = Some(offset);
        } else if trimmed == END_LINE {
            match begin.take() {
                Some(start) => {
                    found = Some(Span {
                        start,
                        end: offset + line.len(),
                    })
                }
                None => return Err(malformed("clauck:end marker without clauck:begin")),
            }
        }
        offset += line.len();
    }
    if begin.is_some() {
        return Err(malformed("clauck:begin marker without clauck:end"));
    }
    Ok(found)
}

fn malformed(detail: &str) -> ClauckError {
    ClauckError::new(
        ErrorCode::Conflict,
        format!("managed block markers are damaged: {detail}"),
    )
    .with_hint("Fix or delete the clauck:begin/clauck:end lines by hand, then rerun.")
}

pub fn insert(doc: Option<&str>, block_lf: &str) -> String {
    match doc {
        None => format!("{block_lf}\n"),
        Some(d) if d.trim().is_empty() => {
            let nl = if uses_crlf(d) { "\r\n" } else { "\n" };
            format!("{}{nl}", with_line_endings(block_lf, uses_crlf(d)))
        }
        Some(d) => {
            let crlf = uses_crlf(d);
            let nl = if crlf { "\r\n" } else { "\n" };
            let sep = if d.ends_with('\n') { "" } else { nl };
            format!("{d}{sep}{nl}{}{nl}", with_line_endings(block_lf, crlf))
        }
    }
}

pub fn replace(doc: &str, span: &Span, block_lf: &str) -> String {
    let crlf = uses_crlf(doc);
    let nl = if crlf { "\r\n" } else { "\n" };
    let had_newline = span.text(doc).ends_with('\n');
    format!(
        "{}{}{}{}",
        &doc[..span.start],
        with_line_endings(block_lf, crlf),
        if had_newline { nl } else { "" },
        &doc[span.end..]
    )
}

pub fn remove(doc: &str, span: &Span) -> String {
    let before = &doc[..span.start];
    let after = &doc[span.end..];
    let before = before
        .strip_suffix("\r\n\r\n")
        .map(|b| format!("{b}\r\n"))
        .or_else(|| before.strip_suffix("\n\n").map(|b| format!("{b}\n")))
        .unwrap_or_else(|| before.to_string());
    format!("{before}{after}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "Follow `.clauck/policy.md`.";

    #[test]
    fn insert_then_remove_restores_original() {
        for original in [
            "# Notes\n\nKeep me.\n",
            "# Notes\r\n\r\nKeep me.\r\n",
            "x\n",
        ] {
            let block = wrap(BODY);
            let doc = insert(Some(original), &block);
            let span = find(&doc).unwrap().unwrap();
            assert_eq!(block_hash(span.text(&doc)), block_hash(&block));
            assert_eq!(remove(&doc, &span), original);
        }
    }

    #[test]
    fn crlf_documents_keep_crlf() {
        let doc = insert(Some("a\r\n"), &wrap(BODY));
        assert!(!doc.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn created_file_removes_to_empty() {
        let doc = insert(None, &wrap(BODY));
        let span = find(&doc).unwrap().unwrap();
        assert_eq!(remove(&doc, &span), "");
    }

    #[test]
    fn human_text_after_block_survives_removal() {
        let doc = insert(Some("top\n"), &wrap(BODY)) + "added later\n";
        let span = find(&doc).unwrap().unwrap();
        assert_eq!(remove(&doc, &span), "top\nadded later\n");
    }

    #[test]
    fn replace_is_idempotent() {
        let doc = insert(Some("top\n"), &wrap("old"));
        let span = find(&doc).unwrap().unwrap();
        let doc2 = replace(&doc, &span, &wrap(BODY));
        let span2 = find(&doc2).unwrap().unwrap();
        assert_eq!(replace(&doc2, &span2, &wrap(BODY)), doc2);
        assert!(doc2.starts_with("top\n\n"));
    }

    #[test]
    fn damaged_markers_are_conflicts() {
        let two = format!("{}\n{}\n", wrap("a"), wrap("b"));
        assert_eq!(find(&two).unwrap_err().code, ErrorCode::Conflict);
        assert!(find(&format!("{BEGIN_LINE}\nx\n")).is_err());
        assert!(find(&format!("x\n{END_LINE}\n")).is_err());
        assert_eq!(find("no markers").unwrap(), None);
    }
}
