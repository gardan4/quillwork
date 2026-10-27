//! Review edits: accept / reject tracked changes ([`review`]), comments ([`comments`]) and table
//! structure ([`tables`]).
//!
//! Every mutating function has the same shape:
//! `(&mut WordDoc, &mut Anchors, alloc_rev, author, date, &Args) -> Result<EditOutcome>`.
//! `alloc_rev` hands out annotation ids from the document's one shared counter (DESIGN §7.1);
//! `author` and `date` stamp every revision and comment written. The anchor map is kept in step
//! with the tree: on return it has been synced, and the outcome lists the block handles the edit
//! changed, inserted and deleted. Untouched XML is never re-serialised.
//!
//! Args structs deserialize from the op objects of `spec/schema/ops.schema.json` (snake_case
//! field names, the common guard fields included and ignored here: the engine checks them).

pub mod comments;
pub mod review;
pub mod tables;

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::anchors::Anchors;
use crate::error::{Error, Result};
use crate::wml::{Loc, PartIdx, WordDoc};
use crate::xml::{Document, NodeId, ns};

/// What one edit did, in anchors.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EditOutcome {
    /// Existing block handles whose content or properties changed.
    pub changed: Vec<String>,
    /// Handles that did not exist before (new paragraphs, new tables).
    pub inserted: Vec<String>,
    /// Handles that no longer resolve.
    pub deleted: Vec<String>,
    /// Revision / annotation ids this edit created.
    pub revisions: Vec<u32>,
}

/// `track` writes tracked changes; `direct` edits in place.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Track,
    Direct,
}

/// The guard fields every mutating op accepts (`Guards` in the op schema). The engine checks
/// `expect` and `base_version`; the edit functions only read `mode`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Guards {
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub expect: Option<serde_json::Value>,
    #[serde(default)]
    pub base_version: Option<u64>,
    #[serde(default)]
    pub mode: Option<Mode>,
}

impl Guards {
    pub fn mode(&self) -> Mode {
        self.mode.unwrap_or_default()
    }
}

/// A spec error (`code` from `spec/errors.md`).
pub(crate) fn err(code: &'static str, msg: impl Into<String>) -> Error {
    Error::Edit { code, message: msg.into() }
}

// ---------------------------------------------------------------------- tree helpers

pub(crate) fn is_w(d: &Document, n: NodeId, local: &str) -> bool {
    d.is(n, ns::W, local)
}

pub(crate) fn w_local(d: &Document, n: NodeId) -> Option<&str> {
    if d.namespace(n) == Some(ns::W) { d.local_name(n) } else { None }
}

pub(crate) fn w_attr(d: &Document, n: NodeId, local: &str) -> Option<String> {
    d.attr_ns(n, ns::W, local).map(|c| c.into_owned())
}

pub(crate) fn parse_hex_id(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 8 {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

/// Pre-order walk over the elements below `root`; `f` returns whether to descend.
pub(crate) fn walk(d: &Document, root: NodeId, mut f: impl FnMut(NodeId) -> bool) {
    let mut cur = d.first_child(root);
    let mut stack: Vec<Option<NodeId>> = Vec::new();
    loop {
        let Some(n) = cur else {
            match stack.pop() {
                Some(resume) => {
                    cur = resume;
                    continue;
                }
                None => break,
            }
        };
        let next = d.next_sibling(n);
        if d.is_element_node(n)
            && f(n)
            && let Some(c) = d.first_child(n)
        {
            stack.push(next);
            cur = Some(c);
            continue;
        }
        cur = next;
    }
}

/// An `mc:AlternateContent` branch readers ignore (a `mc:Fallback` next to a `mc:Choice`, or
/// any `mc:Choice` after the first).
pub(crate) fn is_ignored_branch(d: &Document, n: NodeId) -> bool {
    let fallback = d.is(n, ns::MC, "Fallback");
    let choice = d.is(n, ns::MC, "Choice");
    if !fallback && !choice {
        return false;
    }
    let Some(ac) = d.parent(n) else { return false };
    let first_choice = d.find_child(ac, ns::MC, "Choice");
    if fallback { first_choice.is_some() } else { first_choice != Some(n) }
}

/// Is `n` still in the tree (not inside a detached subtree)?
pub(crate) fn attached(d: &Document, n: NodeId) -> bool {
    let top = d.document_node();
    n == top || d.ancestors(n).any(|a| a == top)
}

/// Nearest `w:p` at or above `n`.
pub(crate) fn enclosing_p(d: &Document, n: NodeId) -> Option<NodeId> {
    if is_w(d, n, "p") {
        return Some(n);
    }
    d.ancestors(n).find(|&a| is_w(d, a, "p"))
}

/// Child elements of `n`, collected (so the tree can be edited while iterating).
pub(crate) fn kids(d: &Document, n: NodeId) -> Vec<NodeId> {
    d.child_elements(n).collect()
}

/// Document-order rank of every node of a part (for range comparisons).
pub(crate) fn order_index(d: &Document) -> std::collections::HashMap<NodeId, usize> {
    d.descendants(d.document_node()).enumerate().map(|(i, n)| (n, i)).collect()
}

/// Qualified-name builder for the `w:` namespace as bound at a part's root.
#[derive(Debug, Clone)]
pub(crate) struct Wq(String);

impl Wq {
    pub(crate) fn of(d: &Document) -> Wq {
        match d.find_prefix(d.root(), ns::W) {
            Some("") => Wq(String::new()),
            Some(p) => Wq(format!("{p}:")),
            None => Wq("w:".into()),
        }
    }
    pub(crate) fn q(&self, local: &str) -> String {
        format!("{}{local}", self.0)
    }
    /// A new detached `w:<local>` with `w:` attributes.
    pub(crate) fn el(&self, d: &mut Document, local: &str, attrs: &[(&str, &str)]) -> Result<NodeId> {
        let n = d.create_element(&self.q(local))?;
        for (k, v) in attrs {
            d.set_attr(n, &self.q(k), v)?;
        }
        Ok(n)
    }
    /// A revision element `w:<local w:id w:author w:date>`.
    pub(crate) fn rev(&self, d: &mut Document, local: &str, id: u32, author: &str, date: &str) -> Result<NodeId> {
        self.el(d, local, &[("id", &id.to_string()), ("author", author), ("date", date)])
    }
}

/// Find the `w:` child of `parent` named `local`, creating it with `make_first` placement when
/// missing (`true` = first child, `false` = last child).
pub(crate) fn child_or_create(d: &mut Document, wq: &Wq, parent: NodeId, local: &str, first: bool) -> Result<NodeId> {
    if let Some(c) = d.find_child(parent, ns::W, local) {
        return Ok(c);
    }
    let c = wq.el(d, local, &[])?;
    if first {
        d.prepend_child(parent, c)?
    } else {
        d.append_child(parent, c)?
    }
    Ok(c)
}

/// The paragraph's `w:pPr`, created as first child when missing.
pub(crate) fn ppr_of(d: &mut Document, wq: &Wq, p: NodeId) -> Result<NodeId> {
    child_or_create(d, wq, p, "pPr", true)
}

/// The paragraph-mark `w:pPr/w:rPr`, created in schema position (before `sectPr`/`pPrChange`).
pub(crate) fn mark_rpr(d: &mut Document, wq: &Wq, p: NodeId) -> Result<NodeId> {
    let ppr = ppr_of(d, wq, p)?;
    if let Some(r) = d.find_child(ppr, ns::W, "rPr") {
        return Ok(r);
    }
    let r = wq.el(d, "rPr", &[])?;
    let before = d.child_elements(ppr).find(|&c| matches!(w_local(d, c), Some("sectPr" | "pPrChange")));
    match before {
        Some(b) => d.insert_before(b, r)?,
        None => d.append_child(ppr, r)?,
    }
    Ok(r)
}

/// Mark a paragraph's mark as inserted (`ins`) or deleted (`del`): `w:pPr/w:rPr/w:<kind>`,
/// first in the mark's `rPr` as the schema wants.
pub(crate) fn mark_paragraph(
    d: &mut Document,
    wq: &Wq,
    p: NodeId,
    kind: &str,
    id: u32,
    author: &str,
    date: &str,
) -> Result<()> {
    let rpr = mark_rpr(d, wq, p)?;
    let m = wq.rev(d, kind, id, author, date)?;
    // ins before del (CT_ParaRPr: ins, del, moveFrom, moveTo).
    let after = d.child_elements(rpr).filter(|&c| kind == "del" && is_w(d, c, "ins")).last();
    match after {
        Some(a) => d.insert_after(a, m)?,
        None => d.prepend_child(rpr, m)?,
    }
    Ok(())
}

/// A clean copy of a run's `w:rPr` for new text: tracked-change markers and `rPrChange` dropped.
pub(crate) fn clone_rpr(d: &mut Document, rpr: NodeId) -> Result<NodeId> {
    let c = d.clone_subtree(rpr)?;
    for k in kids(d, c) {
        if matches!(w_local(d, k), Some("rPrChange" | "ins" | "del" | "moveFrom" | "moveTo")) {
            d.remove(k)?;
        }
    }
    Ok(c)
}

/// A new run: optional `rPr` (used as is), then text as `w:t` / `w:tab` / `w:br`. With
/// `deleted`, text goes into `w:delText`. `xml:space="preserve"` is set where needed.
pub(crate) fn make_run(d: &mut Document, wq: &Wq, rpr: Option<NodeId>, text: &str, deleted: bool) -> Result<NodeId> {
    let r = wq.el(d, "r", &[])?;
    if let Some(rpr) = rpr {
        d.append_child(r, rpr)?;
    }
    let tname = if deleted { "delText" } else { "t" };
    let mut buf = String::new();
    let flush = |d: &mut Document, buf: &mut String| -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let t = wq.el(d, tname, &[])?;
        if buf.starts_with(char::is_whitespace) || buf.ends_with(char::is_whitespace) {
            d.set_attr(t, "xml:space", "preserve")?;
        }
        let tx = d.create_text(buf)?;
        d.append_child(t, tx)?;
        d.append_child(r, t)?;
        buf.clear();
        Ok(())
    };
    for ch in text.chars() {
        match ch {
            '\t' | '\n' => {
                flush(d, &mut buf)?;
                let e = wq.el(d, if ch == '\t' { "tab" } else { "br" }, &[])?;
                d.append_child(r, e)?;
            }
            '\r' => {}
            c => buf.push(c),
        }
    }
    flush(d, &mut buf)?;
    Ok(r)
}

/// Move all children of `n` before it, then remove `n`.
pub(crate) fn unwrap(d: &mut Document, n: NodeId) -> Result<()> {
    while let Some(c) = d.first_child(n) {
        d.insert_before(n, c)?;
    }
    d.remove(n)
}

/// Turn deleted text back into live text below `n` (`delText` → `t`, `delInstrText` → `instrText`).
pub(crate) fn undelete_text(d: &mut Document, wq: &Wq, n: NodeId) -> Result<()> {
    for c in d.descendants(n).collect::<Vec<_>>() {
        match w_local(d, c) {
            Some("delText") => d.set_name(c, &wq.q("t"))?,
            Some("delInstrText") => d.set_name(c, &wq.q("instrText"))?,
            _ => {}
        }
    }
    Ok(())
}

/// Turn live text into deleted text below `n` (`t` → `delText`, `instrText` → `delInstrText`).
pub(crate) fn delete_text(d: &mut Document, wq: &Wq, n: NodeId) -> Result<()> {
    for c in d.descendants(n).collect::<Vec<_>>() {
        match w_local(d, c) {
            Some("t") => d.set_name(c, &wq.q("delText"))?,
            Some("instrText") => d.set_name(c, &wq.q("delInstrText"))?,
            _ => {}
        }
    }
    Ok(())
}

/// Wrap every live run below `container` in `w:del` (consecutive sibling runs share one), as a
/// tracked deletion. Runs already inside `w:del` / `w:moveFrom` are left alone. Returns the ids.
pub(crate) fn track_delete_runs(
    d: &mut Document,
    wq: &Wq,
    container: NodeId,
    alloc: &mut dyn FnMut() -> u32,
    author: &str,
    date: &str,
) -> Result<Vec<u32>> {
    let runs: Vec<NodeId> = d
        .descendants(container)
        .filter(|&r| is_w(d, r, "r"))
        .filter(|&r| {
            !d.ancestors(r)
                .take_while(|&a| a != container)
                .any(|a| matches!(w_local(d, a), Some("del" | "moveFrom" | "r" | "txbxContent")))
        })
        .collect();
    let mut ids = Vec::new();
    let mut i = 0;
    while i < runs.len() {
        let first = runs[i];
        let mut group = vec![first];
        while i + 1 < runs.len() && d.next_sibling_element(group[group.len() - 1]) == Some(runs[i + 1]) {
            group.push(runs[i + 1]);
            i += 1;
        }
        i += 1;
        let id = alloc();
        ids.push(id);
        let del = wq.rev(d, "del", id, author, date)?;
        d.insert_before(first, del)?;
        for r in group {
            delete_text(d, wq, r)?;
            d.append_child(del, r)?;
        }
    }
    Ok(ids)
}

trait NextElement {
    fn next_sibling_element(&self, n: NodeId) -> Option<NodeId>;
}

impl NextElement for Document {
    fn next_sibling_element(&self, n: NodeId) -> Option<NodeId> {
        let mut c = self.next_sibling(n);
        while let Some(x) = c {
            if self.is_element_node(x) {
                return Some(x);
            }
            // whitespace text between runs does not break a group
            if self.text(x).is_some_and(|t| !t.trim().is_empty()) {
                return None;
            }
            c = self.next_sibling(x);
        }
        None
    }
}

/// Does the paragraph hold visible content (text, tabs, breaks, drawings, fields, notes)?
pub(crate) fn has_content(d: &Document, p: NodeId) -> bool {
    d.descendants(p).any(|c| {
        matches!(
            w_local(d, c),
            Some(
                "t" | "delText"
                    | "tab"
                    | "ptab"
                    | "br"
                    | "cr"
                    | "sym"
                    | "drawing"
                    | "pict"
                    | "object"
                    | "fldChar"
                    | "fldSimple"
                    | "footnoteReference"
                    | "endnoteReference"
                    | "noBreakHyphen"
                    | "softHyphen"
            )
        ) || d.is(c, ns::M, "oMath")
            || d.is(c, ns::M, "oMathPara")
    })
}

/// The next annotation id: one past the largest `w:id` on any annotation element in any part
/// (DESIGN §7.1). Callers without the engine's allocator can use this to seed one.
pub fn next_annotation_id(doc: &WordDoc) -> u32 {
    let mut max: i64 = -1;
    for idx in doc.part_indices().collect::<Vec<_>>() {
        let d = doc.part(idx);
        for n in d.descendants(d.root()) {
            if d.namespace(n) != Some(ns::W) || d.attr_count(n) == 0 {
                continue;
            }
            let annotation = matches!(
                d.local_name(n),
                Some(
                    "ins"
                        | "del"
                        | "moveFrom"
                        | "moveTo"
                        | "moveFromRangeStart"
                        | "moveFromRangeEnd"
                        | "moveToRangeStart"
                        | "moveToRangeEnd"
                        | "rPrChange"
                        | "pPrChange"
                        | "sectPrChange"
                        | "tblPrChange"
                        | "tblPrExChange"
                        | "trPrChange"
                        | "tcPrChange"
                        | "tblGridChange"
                        | "numberingChange"
                        | "cellIns"
                        | "cellDel"
                        | "cellMerge"
                        | "comment"
                        | "commentRangeStart"
                        | "commentRangeEnd"
                        | "commentReference"
                        | "bookmarkStart"
                        | "bookmarkEnd"
                        | "customXmlInsRangeStart"
                        | "customXmlDelRangeStart"
                )
            );
            if annotation && let Some(v) = w_attr(d, n, "id").and_then(|v| v.trim().parse::<i64>().ok()) {
                max = max.max(v);
            }
        }
    }
    u32::try_from(max + 1).unwrap_or(0)
}

/// A counter-backed allocator, for callers outside the engine and for tests.
pub fn id_allocator(doc: &WordDoc) -> impl FnMut() -> u32 + use<> {
    let mut next = next_annotation_id(doc);
    move || {
        let v = next;
        next += 1;
        v
    }
}

// ---------------------------------------------------------------------- outcome tracking

/// Snapshot of the handles before an edit; [`Tracker::finish`] syncs the anchors and diffs.
pub(crate) struct Tracker {
    before: HashSet<String>,
    touched: Vec<Loc>,
}

fn all_handles(anchors: &Anchors) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut i = 0;
    while anchors.story_id(i).is_some() {
        out.extend(anchors.handles(i).map(|(h, _)| h.to_owned()));
        i += 1;
    }
    out.extend(anchors.table_anchors().map(|(h, _)| h.to_owned()));
    out
}

impl Tracker {
    pub(crate) fn new(anchors: &Anchors) -> Tracker {
        Tracker { before: all_handles(anchors), touched: Vec::new() }
    }

    /// Record that the paragraph at or above `n` (or the table `n`) changed.
    pub(crate) fn touch(&mut self, doc: &WordDoc, part: PartIdx, n: NodeId) {
        let d = doc.part(part);
        if is_w(d, n, "tbl") {
            self.touched.push(Loc::new(part, n));
        }
        if let Some(p) = enclosing_p(d, n) {
            self.touched.push(Loc::new(part, p));
        }
    }

    pub(crate) fn touch_all_paragraphs(&mut self, doc: &WordDoc, part: PartIdx, n: NodeId) {
        let d = doc.part(part);
        for p in d.descendants(n).filter(|&c| is_w(d, c, "p")) {
            self.touched.push(Loc::new(part, p));
        }
    }

    pub(crate) fn finish(self, doc: &WordDoc, anchors: &mut Anchors, revisions: Vec<u32>) -> EditOutcome {
        anchors.sync(doc);
        let after = all_handles(anchors);
        let mut inserted: Vec<String> = after.difference(&self.before).cloned().collect();
        let mut deleted: Vec<String> = self.before.difference(&after).cloned().collect();
        let mut changed: Vec<String> = Vec::new();
        for loc in &self.touched {
            if !attached(doc.part(loc.part), loc.node) {
                continue;
            }
            if let Some(h) = anchors.handle_of(*loc)
                && self.before.contains(h)
                && !changed.iter().any(|c| c == h)
            {
                changed.push(h.to_owned());
            }
        }
        sort_handles(&mut inserted);
        sort_handles(&mut deleted);
        EditOutcome { changed, inserted, deleted, revisions }
    }
}

fn sort_handles(v: &mut [String]) {
    v.sort_by(|a, b| (a.len(), a).cmp(&(b.len(), b)));
}

#[cfg(test)]
pub(crate) mod test_util {
    //! Build a [`WordDoc`] from inline XML.
    use crate::wml::{MemParts, WordDoc};

    pub const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    pub fn doc_with(body: &str, extra: &[(&str, &str, &str)]) -> WordDoc {
        let mut parts = MemParts::new();
        let document = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {NS} mc:Ignorable="w14"><w:body>{body}</w:body></w:document>"#
        );
        parts.insert("word/document.xml", document.into_bytes());
        let mut rels = String::from(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        );
        for (i, (name, rel_type, xml)) in extra.iter().enumerate() {
            let target = name.trim_start_matches("word/");
            rels.push_str(&format!(r#"<Relationship Id="rIdX{i}" Type="{rel_type}" Target="{target}"/>"#));
            parts.insert(*name, xml.as_bytes().to_vec());
        }
        rels.push_str("</Relationships>");
        parts.insert("word/_rels/document.xml.rels", rels.into_bytes());
        parts.insert(
            "_rels/.rels",
            br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_vec(),
        );
        WordDoc::load(&parts).expect("load")
    }

    pub fn doc(body: &str) -> WordDoc {
        doc_with(body, &[])
    }

    pub fn body_xml(doc: &WordDoc) -> String {
        let d = doc.part(doc.main_part());
        let body = d.child_elements(d.root()).next().unwrap();
        let s = String::from_utf8(d.node_to_bytes(body)).unwrap();
        s.trim_start_matches("<w:body>").trim_end_matches("</w:body>").to_owned()
    }
}
