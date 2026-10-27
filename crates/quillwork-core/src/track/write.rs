//! XML writers for tracked (and direct) edits. Each helper touches only the nodes it is handed.
//!
//! Element order inside `w:rPr` / `w:pPr` follows the schema sequences, so Word never rejects a
//! property we add. New elements are created with the part's own `w:` prefix.

use super::{Stamp, W16DU, ensure_ignorable_ns};
use crate::Result;
use crate::xml::{Document, NodeId, ns};

/// `CT_RPr` / `CT_ParaRPr` sequence (revision markers of a paragraph mark come first).
pub const RPR_ORDER: &[&str] = &[
    "ins",
    "del",
    "moveFrom",
    "moveTo",
    "rStyle",
    "rFonts",
    "b",
    "bCs",
    "i",
    "iCs",
    "caps",
    "smallCaps",
    "strike",
    "dstrike",
    "outline",
    "shadow",
    "emboss",
    "imprint",
    "noProof",
    "snapToGrid",
    "vanish",
    "webHidden",
    "color",
    "spacing",
    "w",
    "kern",
    "position",
    "sz",
    "szCs",
    "highlight",
    "u",
    "effect",
    "bdr",
    "shd",
    "fitText",
    "vertAlign",
    "rtl",
    "cs",
    "em",
    "lang",
    "eastAsianLayout",
    "specVanish",
    "oMath",
    "rPrChange",
];

/// `CT_PPr` sequence.
pub const PPR_ORDER: &[&str] = &[
    "pStyle",
    "keepNext",
    "keepLines",
    "pageBreakBefore",
    "framePr",
    "widowControl",
    "numPr",
    "suppressLineNumbers",
    "pBdr",
    "shd",
    "tabs",
    "suppressAutoHyphens",
    "kinsoku",
    "wordWrap",
    "overflowPunct",
    "topLinePunct",
    "autoSpaceDE",
    "autoSpaceDN",
    "bidi",
    "adjustRightInd",
    "snapToGrid",
    "spacing",
    "ind",
    "contextualSpacing",
    "mirrorIndents",
    "suppressOverlap",
    "jc",
    "textDirection",
    "textAlignment",
    "textboxTightWrap",
    "outlineLvl",
    "divId",
    "cnfStyle",
    "rPr",
    "sectPr",
    "pPrChange",
];

/// Children of a run that never travel into a copied `rPr`.
const RPR_REVISION_CHILDREN: &[&str] = &["ins", "del", "moveFrom", "moveTo", "rPrChange"];

// ------------------------------------------------------------------ small helpers

/// The part's prefix for the `w:` namespace (`w` almost always).
pub fn w_prefix(d: &Document) -> String {
    d.find_prefix(d.root(), ns::W).unwrap_or("w").to_owned()
}

/// Qualified name `w:<local>` with the part's prefix.
pub fn wq(d: &Document, local: &str) -> String {
    let p = w_prefix(d);
    if p.is_empty() { local.to_owned() } else { format!("{p}:{local}") }
}

/// A detached `w:<local>` element.
pub fn new_w(d: &mut Document, local: &str) -> Result<NodeId> {
    let q = wq(d, local);
    d.create_element(&q)
}

/// Set `w:<local>` on `n` (works on detached elements: the prefix is written literally).
pub fn set_w(d: &mut Document, n: NodeId, local: &str, value: &str) -> Result<()> {
    let q = wq(d, local);
    d.set_attr(n, &q, value)
}

/// Remove `w:<local>` from `n`.
pub fn remove_w(d: &mut Document, n: NodeId, local: &str) -> Result<bool> {
    let q = wq(d, local);
    d.remove_attr(n, &q)
}

/// `w:<local>`?
pub fn is_w(d: &Document, n: NodeId, local: &str) -> bool {
    d.is(n, ns::W, local)
}

/// Local name of a `w:` element.
pub fn w_local(d: &Document, n: NodeId) -> Option<&str> {
    if d.namespace(n) == Some(ns::W) { d.local_name(n) } else { None }
}

/// `@w:<local>` of `n`.
pub fn w_attr(d: &Document, n: NodeId, local: &str) -> Option<String> {
    d.attr_ns(n, ns::W, local).map(|c| c.into_owned())
}

/// First child `w:<local>`.
pub fn w_child(d: &Document, n: NodeId, local: &str) -> Option<NodeId> {
    d.find_child(n, ns::W, local)
}

/// Element children of `n`.
pub fn elements(d: &Document, n: NodeId) -> Vec<NodeId> {
    d.child_elements(n).collect()
}

/// Insert `child` into `parent` at its place in `order` (schema sequence). Unknown names go last
/// (before a trailing `*Change`).
pub fn insert_ordered(d: &mut Document, parent: NodeId, child: NodeId, order: &[&str]) -> Result<()> {
    let local = d.local_name(child).unwrap_or("").to_owned();
    let rank = |l: &str| order.iter().position(|o| *o == l);
    let mine = rank(&local).unwrap_or(order.len() - 1);
    for c in elements(d, parent) {
        let theirs = match w_local(d, c) {
            Some(l) => rank(l).unwrap_or(usize::MAX),
            None => usize::MAX,
        };
        if theirs != usize::MAX && theirs > mine {
            return d.insert_before(c, child);
        }
    }
    d.append_child(parent, child)
}

/// The `w:<local>` child of `parent`, created in schema order when missing.
pub fn ensure_child(d: &mut Document, parent: NodeId, local: &str, order: &[&str]) -> Result<NodeId> {
    if let Some(c) = w_child(d, parent, local) {
        return Ok(c);
    }
    let c = new_w(d, local)?;
    insert_ordered(d, parent, c, order)?;
    Ok(c)
}

/// The paragraph's `w:pPr`, created when missing.
pub fn ensure_ppr(d: &mut Document, p: NodeId) -> Result<NodeId> {
    if let Some(ppr) = w_child(d, p, "pPr") {
        return Ok(ppr);
    }
    let ppr = new_w(d, "pPr")?;
    d.prepend_child(p, ppr)?;
    Ok(ppr)
}

/// The run's `w:rPr`, created when missing.
pub fn ensure_rpr(d: &mut Document, r: NodeId) -> Result<NodeId> {
    if let Some(rpr) = w_child(d, r, "rPr") {
        return Ok(rpr);
    }
    let rpr = new_w(d, "rPr")?;
    d.prepend_child(r, rpr)?;
    Ok(rpr)
}

/// Paragraph-mark `w:pPr/w:rPr`, if any.
pub fn mark_rpr(d: &Document, p: NodeId) -> Option<NodeId> {
    w_child(d, p, "pPr").and_then(|ppr| w_child(d, ppr, "rPr"))
}

// ------------------------------------------------------------------ revision elements

/// A detached revision element `w:<local>` with `w:id`, `w:author`, `w:date` and
/// `w16du:dateUtc` (namespace declared on the part root and made ignorable).
pub fn revision_el(d: &mut Document, local: &str, id: u32, stamp: &Stamp) -> Result<NodeId> {
    let du = ensure_ignorable_ns(d, W16DU, "w16du")?;
    let el = new_w(d, local)?;
    set_w(d, el, "id", &id.to_string())?;
    set_w(d, el, "author", &stamp.author)?;
    set_w(d, el, "date", &stamp.date)?;
    d.set_attr(el, &format!("{du}:dateUtc"), &stamp.date)?;
    Ok(el)
}

/// Rename the text of a run to its deleted form (`w:t` → `w:delText`, `w:instrText` →
/// `w:delInstrText`), anywhere below `n`.
pub fn to_deleted_text(d: &mut Document, n: NodeId) -> Result<()> {
    let nodes: Vec<NodeId> = d.descendants(n).collect();
    for c in nodes {
        match w_local(d, c) {
            Some("t") => {
                let q = wq(d, "delText");
                d.set_name(c, &q)?;
            }
            Some("instrText") => {
                let q = wq(d, "delInstrText");
                d.set_name(c, &q)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Wrap a contiguous group of sibling inline nodes (runs, usually) in one new `w:<wrapper>`,
/// placed where the first one was. Returns the wrapper.
pub fn wrap(d: &mut Document, nodes: &[NodeId], wrapper: NodeId) -> Result<NodeId> {
    let first = *nodes.first().expect("wrap: no nodes");
    d.insert_before(first, wrapper)?;
    for &n in nodes {
        d.append_child(wrapper, n)?;
    }
    Ok(wrapper)
}

/// Mark a group of sibling runs as deleted: one `w:del`, text renamed. Returns the `w:del`.
pub fn delete_runs(d: &mut Document, runs: &[NodeId], id: u32, stamp: &Stamp) -> Result<NodeId> {
    for &r in runs {
        to_deleted_text(d, r)?;
    }
    let del = revision_el(d, "del", id, stamp)?;
    wrap(d, runs, del)
}

/// A fresh run with a copy of `rpr` (revision markers stripped) holding `text`: `\t` becomes
/// `w:tab`, `\n` a `w:br`, the rest `w:t` (with `xml:space="preserve"` when it needs it).
pub fn make_run(d: &mut Document, rpr: Option<NodeId>, text: &str) -> Result<NodeId> {
    let r = new_w(d, "r")?;
    if let Some(src) = rpr {
        let copy = clone_rpr(d, src)?;
        if d.first_child(copy).is_some() {
            d.append_child(r, copy)?;
        }
    }
    append_text(d, r, text)?;
    Ok(r)
}

/// Append `text` to run `r` as `w:t` / `w:tab` / `w:br` children.
pub fn append_text(d: &mut Document, r: NodeId, text: &str) -> Result<()> {
    let mut buf = String::new();
    let flush = |d: &mut Document, buf: &mut String| -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let t = new_w(d, "t")?;
        if buf.starts_with(char::is_whitespace) || buf.ends_with(char::is_whitespace) || buf.contains("  ") {
            d.set_attr(t, "xml:space", "preserve")?;
        }
        let tx = d.create_text(buf)?;
        d.append_child(t, tx)?;
        d.append_child(r, t)?;
        buf.clear();
        Ok(())
    };
    for c in text.chars() {
        match c {
            '\t' | '\n' => {
                flush(d, &mut buf)?;
                let el = new_w(d, if c == '\t' { "tab" } else { "br" })?;
                d.append_child(r, el)?;
            }
            '\u{2011}' => {
                flush(d, &mut buf)?;
                let el = new_w(d, "noBreakHyphen")?;
                d.append_child(r, el)?;
            }
            '\u{00AD}' => {
                flush(d, &mut buf)?;
                let el = new_w(d, "softHyphen")?;
                d.append_child(r, el)?;
            }
            _ => buf.push(c),
        }
    }
    flush(d, &mut buf)
}

/// A detached `w:rPr` copy of `src` (a run's or a paragraph mark's rPr) without revision markers.
pub fn clone_rpr(d: &mut Document, src: NodeId) -> Result<NodeId> {
    let copy = d.clone_subtree(src)?;
    for c in elements(d, copy) {
        if w_local(d, c).is_some_and(|l| RPR_REVISION_CHILDREN.contains(&l)) {
            d.remove(c)?;
        }
    }
    // A paragraph mark's rPr and a run's rPr share the element name; nothing else to rename.
    Ok(copy)
}

/// Mark the paragraph mark of `p` as inserted (`kind = "ins"`) or deleted (`"del"`).
/// An existing mark revision of the other kind written by someone else stays (Word nests them).
pub fn mark_paragraph(d: &mut Document, p: NodeId, kind: &str, id: u32, stamp: &Stamp) -> Result<NodeId> {
    let ppr = ensure_ppr(d, p)?;
    let rpr = ensure_child(d, ppr, "rPr", PPR_ORDER)?;
    if let Some(existing) = w_child(d, rpr, kind) {
        return Ok(existing);
    }
    let el = revision_el(d, kind, id, stamp)?;
    insert_ordered(d, rpr, el, RPR_ORDER)?;
    Ok(el)
}

/// The mark revision (`ins`/`del`) of `p`, if any.
pub fn mark_revision(d: &Document, p: NodeId, kind: &str) -> Option<NodeId> {
    mark_rpr(d, p).and_then(|rpr| w_child(d, rpr, kind))
}

// ------------------------------------------------------------------ property changes

/// Before changing a run's (or paragraph mark's) `rPr`, snapshot it into `w:rPrChange`.
/// Returns `true` when a snapshot already existed (ours or someone else's) and was kept.
pub fn snapshot_rpr(d: &mut Document, rpr: NodeId, id: u32, stamp: &Stamp) -> Result<bool> {
    if w_child(d, rpr, "rPrChange").is_some() {
        return Ok(true);
    }
    let old = d.clone_subtree(rpr)?;
    for c in elements(d, old) {
        if w_local(d, c).is_some_and(|l| RPR_REVISION_CHILDREN.contains(&l)) {
            d.remove(c)?;
        }
    }
    strip_attrs(d, old)?;
    let ch = revision_el(d, "rPrChange", id, stamp)?;
    d.append_child(ch, old)?;
    insert_ordered(d, rpr, ch, RPR_ORDER)?;
    Ok(false)
}

/// Before changing a paragraph's `pPr`, snapshot it (minus `rPr`, `sectPr`, `pPrChange`) into
/// `w:pPrChange`. Returns `true` when a snapshot already existed and was kept.
pub fn snapshot_ppr(d: &mut Document, ppr: NodeId, id: u32, stamp: &Stamp) -> Result<bool> {
    if w_child(d, ppr, "pPrChange").is_some() {
        return Ok(true);
    }
    let old = d.clone_subtree(ppr)?;
    for c in elements(d, old) {
        if matches!(w_local(d, c), Some("rPr" | "sectPr" | "pPrChange")) {
            d.remove(c)?;
        }
    }
    strip_attrs(d, old)?;
    let ch = revision_el(d, "pPrChange", id, stamp)?;
    d.append_child(ch, old)?;
    insert_ordered(d, ppr, ch, PPR_ORDER)?;
    Ok(false)
}

/// Drop every attribute of `n` (a snapshot element carries none).
fn strip_attrs(d: &mut Document, n: NodeId) -> Result<()> {
    let names: Vec<String> = d.attrs(n).map(|(q, _)| q.to_owned()).collect();
    for q in names {
        d.remove_attr(n, &q)?;
    }
    Ok(())
}

/// Strip `w14:paraId` / `w14:textId` (a cloned paragraph must not share them).
pub fn strip_para_ids(d: &mut Document, p: NodeId) -> Result<()> {
    let names: Vec<String> = d
        .attrs(p)
        .filter(|(q, _)| q.ends_with(":paraId") || q.ends_with(":textId"))
        .map(|(q, _)| q.to_owned())
        .collect();
    for q in names {
        d.remove_attr(p, &q)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Hallo wereld</w:t></w:r></w:p></w:body></w:document>"#;

    #[test]
    fn delete_and_insert_runs() {
        let mut d = Document::parse(DOC.as_bytes()).unwrap();
        let r = d.descendants(d.root()).find(|&n| is_w(&d, n, "r")).unwrap();
        let rpr = w_child(&d, r, "rPr");
        let stamp = Stamp::new("AI", 0);
        let del = delete_runs(&mut d, &[r], 5, &stamp).unwrap();
        let nr = make_run(&mut d, rpr, " Dag\twereld").unwrap();
        let ins = revision_el(&mut d, "ins", 6, &stamp).unwrap();
        d.append_child(ins, nr).unwrap();
        d.insert_after(del, ins).unwrap();
        let s = String::from_utf8(d.to_bytes()).unwrap();
        assert!(s.contains("w:delText>Hallo wereld</w:delText>"), "{s}");
        assert!(s.contains(r#"<w:t xml:space="preserve"> Dag</w:t><w:tab/><w:t>wereld</w:t>"#), "{s}");
        assert!(s.contains(r#"w16du:dateUtc="1970-01-01T00:00:00Z""#), "{s}");
        assert!(s.contains(r#"mc:Ignorable="w16du""#), "{s}");
    }

    #[test]
    fn snapshots_and_order() {
        let mut d = Document::parse(DOC.as_bytes()).unwrap();
        let r = d.descendants(d.root()).find(|&n| is_w(&d, n, "r")).unwrap();
        let rpr = w_child(&d, r, "rPr").unwrap();
        let stamp = Stamp::new("AI", 0);
        assert!(!snapshot_rpr(&mut d, rpr, 1, &stamp).unwrap());
        let i = new_w(&mut d, "i").unwrap();
        insert_ordered(&mut d, rpr, i, RPR_ORDER).unwrap();
        assert!(snapshot_rpr(&mut d, rpr, 2, &stamp).unwrap());
        let s = String::from_utf8(d.to_bytes()).unwrap();
        assert!(s.contains("<w:rPr><w:b/><w:i/><w:rPrChange"), "{s}");
        assert!(s.contains("<w:rPr><w:b/></w:rPr></w:rPrChange>"), "{s}");
    }
}
