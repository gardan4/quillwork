//! Shared helpers for the `wml_*` / `anchors_*` tests: tiny in-memory documents.
#![allow(dead_code)]

use quillwork_core::wml::{MemParts, WordDoc};

pub const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// A document whose body is `body` (inner XML of `w:body`), with optional `numbering.xml` and
/// `styles.xml` contents (inner XML of the root element).
pub fn mini(body: &str, numbering: Option<&str>, styles: Option<&str>) -> WordDoc {
    let mut rels = String::new();
    let mut p = MemParts::new();
    p.insert(
        "word/document.xml",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}" xmlns:r="{REL}"><w:body>{body}</w:body></w:document>"#
        )
        .into_bytes(),
    );
    if let Some(n) = numbering {
        rels.push_str(&format!(r#"<Relationship Id="rIdN" Type="{REL}/numbering" Target="numbering.xml"/>"#));
        p.insert("word/numbering.xml", format!(r#"<w:numbering xmlns:w="{W}">{n}</w:numbering>"#).into_bytes());
    }
    if let Some(s) = styles {
        rels.push_str(&format!(r#"<Relationship Id="rIdS" Type="{REL}/styles" Target="styles.xml"/>"#));
        p.insert("word/styles.xml", format!(r#"<w:styles xmlns:w="{W}">{s}</w:styles>"#).into_bytes());
    }
    p.insert(
        "word/_rels/document.xml.rels",
        format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#)
            .into_bytes(),
    );
    WordDoc::load(&p).expect("load mini document")
}

/// `<w:p>` with optional numPr and style, holding `text`.
pub fn p(text: &str, num: Option<(i64, i64)>, style: Option<&str>) -> String {
    let mut ppr = String::new();
    if let Some(s) = style {
        ppr.push_str(&format!(r#"<w:pStyle w:val="{s}"/>"#));
    }
    if let Some((id, lvl)) = num {
        ppr.push_str(&format!(r#"<w:numPr><w:ilvl w:val="{lvl}"/><w:numId w:val="{id}"/></w:numPr>"#));
    }
    let ppr = if ppr.is_empty() { String::new() } else { format!("<w:pPr>{ppr}</w:pPr>") };
    format!("<w:p>{ppr}<w:r><w:t>{text}</w:t></w:r></w:p>")
}

/// A `w:lvl`.
pub fn lvl(ilvl: usize, start: i64, fmt: &str, text: &str, extra: &str) -> String {
    format!(
        r#"<w:lvl w:ilvl="{ilvl}"><w:start w:val="{start}"/><w:numFmt w:val="{fmt}"/><w:lvlText w:val="{text}"/>{extra}</w:lvl>"#
    )
}
