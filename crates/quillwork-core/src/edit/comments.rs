//! Comments: add (on a block or a found range), reply, resolve, edit, delete, list
//! (DESIGN §6.2).
//!
//! A comment is spread over up to five parts: `comments.xml` (the text), `commentsExtended.xml`
//! (`w15:commentEx`: reply threading through `paraIdParent`, the done flag), `commentsIds.xml`
//! (`w16cid:commentId`: the durable id), `commentsExtensible.xml` (`w16cex:commentExtensible`:
//! the UTC date) and `people.xml` (the author). Missing parts are created, with their
//! relationship and content type. In the story the comment covers a range
//! (`commentRangeStart` … `commentRangeEnd`) followed by a reference run.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{
    EditOutcome, Guards, Tracker, Wq, attached, enclosing_p, err, is_w, kids, order_index, parse_hex_id, w_attr,
    w_local,
};
use crate::anchors::{Anchors, Target};
use crate::error::Result;
use crate::wml::{Loc, PartIdx, PartKind, StoryKind, TextMode, WordDoc, text};
use crate::xml::{Document, NodeId, ns};

// ---------------------------------------------------------------------- args and results

/// `add_comment`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AddCommentArgs {
    #[serde(flatten)]
    pub guards: Guards,
    pub anchor: String,
    #[serde(default)]
    pub find: Option<String>,
    #[serde(default)]
    pub occurrence: Option<u32>,
    pub text: String,
    #[serde(default)]
    pub author: Option<String>,
}

/// `reply_comment`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReplyCommentArgs {
    #[serde(flatten)]
    pub guards: Guards,
    pub comment: String,
    pub text: String,
    #[serde(default)]
    pub author: Option<String>,
}

/// `resolve_comment`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResolveCommentArgs {
    #[serde(flatten)]
    pub guards: Guards,
    pub comment: String,
    pub done: bool,
}

/// `delete_comment`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeleteCommentArgs {
    #[serde(flatten)]
    pub guards: Guards,
    pub comment: String,
}

/// `edit_comment` (proposed op, see `docs/REVIEW_NOTES.md`): replace a comment's text.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EditCommentArgs {
    #[serde(flatten)]
    pub guards: Guards,
    pub comment: String,
    pub text: String,
}

/// `list_comments`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListCommentsArgs {
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub spec_version: Option<String>,
    #[serde(default)]
    pub open_only: Option<bool>,
}

/// One comment of `list_comments`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommentInfo {
    /// Comment anchor (`c1`, …).
    pub id: String,
    pub author: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initials: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub done: bool,
    /// The commented text (accepted view).
    pub on: String,
    /// The block the range starts in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    pub text: String,
    pub replies: Vec<CommentInfo>,
}

/// `list_comments` result.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CommentList {
    pub comments: Vec<CommentInfo>,
}

// ---------------------------------------------------------------------- parts

const CT_BASE: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.";

struct PartSpec {
    kind: PartKind,
    base: &'static str,
    root: &'static str,
    rel: &'static str,
    ct: &'static str,
}

const SPECS: [PartSpec; 5] = [
    PartSpec {
        kind: PartKind::Comments,
        base: "comments",
        root: "w:comments",
        rel: "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments",
        ct: "comments+xml",
    },
    PartSpec {
        kind: PartKind::CommentsExtended,
        base: "commentsExtended",
        root: "w15:commentsEx",
        rel: "http://schemas.microsoft.com/office/2011/relationships/commentsExtended",
        ct: "commentsExtended+xml",
    },
    PartSpec {
        kind: PartKind::CommentsIds,
        base: "commentsIds",
        root: "w16cid:commentsIds",
        rel: "http://schemas.microsoft.com/office/2016/09/relationships/commentsIds",
        ct: "commentsIds+xml",
    },
    PartSpec {
        kind: PartKind::CommentsExtensible,
        base: "commentsExtensible",
        root: "w16cex:commentsExtensible",
        rel: "http://schemas.microsoft.com/office/2018/08/relationships/commentsExtensible",
        ct: "commentsExtensible+xml",
    },
    PartSpec {
        kind: PartKind::People,
        base: "people",
        root: "w15:people",
        rel: "http://schemas.microsoft.com/office/2011/relationships/people",
        ct: "people+xml",
    },
];

fn part_of(doc: &WordDoc, kind: PartKind) -> Option<PartIdx> {
    doc.parts_of(kind).next()
}

/// The part of `kind`, created (empty, with relationship and content type) when missing.
fn ensure_part(doc: &mut WordDoc, kind: PartKind) -> Result<PartIdx> {
    if let Some(p) = part_of(doc, kind) {
        return Ok(p);
    }
    let spec = SPECS.iter().find(|s| s.kind == kind).expect("comment part kind");
    let taken =
        |doc: &WordDoc, name: &str| doc.part_idx(name).is_some() || doc.package().is_some_and(|p| p.contains(name));
    let mut name = format!("word/{}.xml", spec.base);
    let mut i = 1;
    while taken(doc, &name) {
        name = format!("word/{}{i}.xml", spec.base);
        i += 1;
    }
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<{root} xmlns:mc="{mc}" xmlns:w="{w}" xmlns:w14="{w14}" xmlns:w15="{w15}" xmlns:w16cid="{cid}" xmlns:w16cex="{cex}" mc:Ignorable="w14 w15 w16cid w16cex"></{root}>"#,
        root = spec.root,
        mc = ns::MC,
        w = ns::W,
        w14 = ns::W14,
        w15 = ns::W15,
        cid = ns::W16CID,
        cex = ns::W16CEX,
    );
    doc.add_part(&name, kind, spec.rel, &format!("{CT_BASE}{}", spec.ct), xml.into_bytes())
}

/// Prefix bound to `uri` at the root of `d`, declaring `want` there when unbound.
fn prefix(d: &mut Document, uri: &str, want: &str) -> Result<String> {
    let root = d.root();
    if let Some(p) = d.find_prefix(root, uri) {
        return Ok(p.to_owned());
    }
    d.set_attr(root, &format!("xmlns:{want}"), uri)?;
    Ok(want.to_owned())
}

fn q(p: &str, local: &str) -> String {
    if p.is_empty() { local.to_owned() } else { format!("{p}:{local}") }
}

/// Elements of `local` in namespace `uri` directly below the root.
fn entries(d: &Document, uri: &str, local: &str) -> Vec<NodeId> {
    d.child_elements(d.root()).filter(|&c| d.is(c, uri, local)).collect()
}

fn find_entry(d: &Document, uri: &str, local: &str, attr: &str, val: u32) -> Option<NodeId> {
    entries(d, uri, local).into_iter().find(|&c| d.attr_ns(c, uri, attr).and_then(|v| parse_hex_id(&v)) == Some(val))
}

// ---------------------------------------------------------------------- comment lookup

struct CommentRef {
    part: PartIdx,
    node: NodeId,
}

fn comment_by_anchor(doc: &WordDoc, anchors: &Anchors, handle: &str) -> Result<CommentRef> {
    match anchors.resolve(doc, handle) {
        Some(Target::Comment { comment, .. }) => Ok(CommentRef { part: comment.part, node: comment.node }),
        _ => Err(err("NOT_FOUND", format!("no comment {handle}"))),
    }
}

/// The comment's key paraId (its last paragraph's), stamped when missing.
fn key_para_id(doc: &mut WordDoc, part: PartIdx, comment: NodeId) -> Result<Option<u32>> {
    let d = doc.part(part);
    let Some(last) = d.descendants(comment).filter(|&c| is_w(d, c, "p")).last() else { return Ok(None) };
    Ok(Some(doc.ensure_para_id(Loc::new(part, last))?))
}

fn read_key_para_id(d: &Document, comment: NodeId) -> Option<u32> {
    let last = d.descendants(comment).filter(|&c| is_w(d, c, "p")).last()?;
    d.attr_ns(last, ns::W14, "paraId").and_then(|v| parse_hex_id(&v))
}

/// Story anchor of the comment element at `node`.
fn comment_handle(doc: &WordDoc, anchors: &Anchors, part: PartIdx, node: NodeId) -> Option<String> {
    let i = doc.stories().iter().position(|s| s.part == part && s.root == node)?;
    anchors.story_id(i).map(str::to_owned)
}

fn has_style(doc: &WordDoc, id: &str) -> bool {
    let Some(sp) = doc.styles_part() else { return false };
    let d = doc.part(sp);
    d.child_elements(d.root()).any(|s| is_w(d, s, "style") && w_attr(d, s, "styleId").as_deref() == Some(id))
}

fn initials(author: &str) -> String {
    let s: String = author.split_whitespace().filter_map(|w| w.chars().find(|c| c.is_alphanumeric())).collect();
    if s.is_empty() { author.chars().take(2).collect() } else { s.to_uppercase() }
}

/// Every part a comment range or reference can live in.
fn story_parts(doc: &WordDoc) -> Vec<PartIdx> {
    let mut v = Vec::new();
    for s in doc.stories() {
        if !v.contains(&s.part) && !matches!(s.kind, StoryKind::Comment { .. }) {
            v.push(s.part);
        }
    }
    v
}

// ---------------------------------------------------------------------- building

/// The reference run `<w:r><w:rPr><w:rStyle CommentReference/></w:rPr><w:commentReference/></w:r>`.
fn reference_run(d: &mut Document, wq: &Wq, id: &str, styled: bool) -> Result<NodeId> {
    let r = wq.el(d, "r", &[])?;
    if styled {
        let rpr = wq.el(d, "rPr", &[])?;
        let rs = wq.el(d, "rStyle", &[("val", "CommentReference")])?;
        d.append_child(rpr, rs)?;
        d.append_child(r, rpr)?;
    }
    let cref = wq.el(d, "commentReference", &[("id", id)])?;
    d.append_child(r, cref)?;
    Ok(r)
}

/// Fill a `w:comment` with paragraphs for `body` (one per line), annotationRef run first.
fn fill_comment(doc: &mut WordDoc, part: PartIdx, comment: NodeId, body: &str, keep_ppr: Option<NodeId>) -> Result<()> {
    let text_style = has_style(doc, "CommentText");
    let ref_style = has_style(doc, "CommentReference");
    let d = doc.part_mut(part);
    let wq = Wq::of(d);
    let lines: Vec<&str> = body.split('\n').collect();
    let mut paras = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let p = wq.el(d, "p", &[])?;
        match keep_ppr {
            Some(src) => {
                let c = d.clone_subtree(src)?;
                d.append_child(p, c)?;
            }
            None if text_style => {
                let ppr = wq.el(d, "pPr", &[])?;
                let ps = wq.el(d, "pStyle", &[("val", "CommentText")])?;
                d.append_child(ppr, ps)?;
                d.append_child(p, ppr)?;
            }
            None => {}
        }
        if i == 0 {
            let r = wq.el(d, "r", &[])?;
            if ref_style {
                let rpr = wq.el(d, "rPr", &[])?;
                let rs = wq.el(d, "rStyle", &[("val", "CommentReference")])?;
                d.append_child(rpr, rs)?;
                d.append_child(r, rpr)?;
            }
            let a = wq.el(d, "annotationRef", &[])?;
            d.append_child(r, a)?;
            d.append_child(p, r)?;
        }
        let line = line.trim_end_matches('\r');
        if !line.is_empty() {
            let r = super::make_run(d, &wq, None, line, false)?;
            d.append_child(p, r)?;
        }
        d.append_child(comment, p)?;
        paras.push(p);
    }
    for p in paras {
        doc.ensure_para_id(Loc::new(part, p))?;
    }
    Ok(())
}

/// A deterministic durable id: non-zero, below `0x7FFFFFFF`, not yet used.
fn durable_id(doc: &WordDoc, seed: u32) -> u32 {
    let used: HashSet<u32> = part_of(doc, PartKind::CommentsIds)
        .map(|p| {
            let d = doc.part(p);
            entries(d, ns::W16CID, "commentId")
                .into_iter()
                .filter_map(|c| d.attr_ns(c, ns::W16CID, "durableId").and_then(|v| parse_hex_id(&v)))
                .collect()
        })
        .unwrap_or_default();
    let mut h = seed.wrapping_mul(0x9E37_79B1) ^ 0x5bd1_e995;
    loop {
        h ^= h >> 15;
        h = h.wrapping_mul(0x2c1b_3c6d);
        h ^= h >> 12;
        let v = h & 0x7FFF_FFFE;
        if v != 0 && !used.contains(&v) {
            return v;
        }
        h = h.wrapping_add(1);
    }
}

/// Write the extension entries of a new comment: commentEx, commentId, commentExtensible,
/// person.
fn add_extensions(doc: &mut WordDoc, para_id: u32, parent: Option<u32>, author: &str, date: &str) -> Result<()> {
    // commentsExtended
    let ext = ensure_part(doc, PartKind::CommentsExtended)?;
    {
        let d = doc.part_mut(ext);
        let p15 = prefix(d, ns::W15, "w15")?;
        if let Some(parent) = parent
            && find_entry(d, ns::W15, "commentEx", "paraId", parent).is_none()
        {
            let e = d.create_element(&q(&p15, "commentEx"))?;
            d.set_attr(e, &q(&p15, "paraId"), &format!("{parent:08X}"))?;
            d.set_attr(e, &q(&p15, "done"), "0")?;
            let root = d.root();
            d.append_child(root, e)?;
        }
        let e = d.create_element(&q(&p15, "commentEx"))?;
        d.set_attr(e, &q(&p15, "paraId"), &format!("{para_id:08X}"))?;
        if let Some(parent) = parent {
            d.set_attr(e, &q(&p15, "paraIdParent"), &format!("{parent:08X}"))?;
        }
        d.set_attr(e, &q(&p15, "done"), "0")?;
        let root = d.root();
        d.append_child(root, e)?;
    }
    // commentsIds + commentsExtensible
    let durable = durable_id(doc, para_id);
    let ids = ensure_part(doc, PartKind::CommentsIds)?;
    {
        let d = doc.part_mut(ids);
        let p = prefix(d, ns::W16CID, "w16cid")?;
        let e = d.create_element(&q(&p, "commentId"))?;
        d.set_attr(e, &q(&p, "paraId"), &format!("{para_id:08X}"))?;
        d.set_attr(e, &q(&p, "durableId"), &format!("{durable:08X}"))?;
        let root = d.root();
        d.append_child(root, e)?;
    }
    let cex = ensure_part(doc, PartKind::CommentsExtensible)?;
    {
        let d = doc.part_mut(cex);
        let p = prefix(d, ns::W16CEX, "w16cex")?;
        let e = d.create_element(&q(&p, "commentExtensible"))?;
        d.set_attr(e, &q(&p, "durableId"), &format!("{durable:08X}"))?;
        d.set_attr(e, &q(&p, "dateUtc"), date)?;
        let root = d.root();
        d.append_child(root, e)?;
    }
    // people
    let people = ensure_part(doc, PartKind::People)?;
    {
        let d = doc.part_mut(people);
        let p15 = prefix(d, ns::W15, "w15")?;
        let known = entries(d, ns::W15, "person")
            .into_iter()
            .any(|c| d.attr_ns(c, ns::W15, "author").as_deref() == Some(author));
        if !known {
            let person = d.create_element(&q(&p15, "person"))?;
            d.set_attr(person, &q(&p15, "author"), author)?;
            let pi = d.create_element(&q(&p15, "presenceInfo"))?;
            d.set_attr(pi, &q(&p15, "providerId"), "None")?;
            d.set_attr(pi, &q(&p15, "userId"), author)?;
            d.append_child(person, pi)?;
            let root = d.root();
            d.append_child(root, person)?;
        }
    }
    Ok(())
}

/// A fresh comment `w:id`: from the shared allocator, skipping ids comments already use.
fn fresh_comment_id(doc: &WordDoc, alloc: &mut dyn FnMut() -> u32) -> u32 {
    let used: HashSet<String> = part_of(doc, PartKind::Comments)
        .map(|p| {
            let d = doc.part(p);
            d.child_elements(d.root()).filter_map(|c| w_attr(d, c, "id").map(|v| v.trim().to_owned())).collect()
        })
        .unwrap_or_default();
    loop {
        let id = alloc();
        if !used.contains(&id.to_string()) {
            return id;
        }
    }
}

/// Create the `w:comment` element (inserted after `after`, or appended) and its extension
/// entries. Returns (part, node, key paraId).
#[allow(clippy::too_many_arguments)]
fn create_comment(
    doc: &mut WordDoc,
    id: u32,
    author: &str,
    date: &str,
    body: &str,
    after: Option<NodeId>,
    parent_para: Option<u32>,
) -> Result<(PartIdx, NodeId, u32)> {
    let part = ensure_part(doc, PartKind::Comments)?;
    let node = {
        let d = doc.part_mut(part);
        let wq = Wq::of(d);
        let c = wq.el(
            d,
            "comment",
            &[("id", &id.to_string()), ("author", author), ("date", date), ("initials", &initials(author))],
        )?;
        match after {
            Some(a) => d.insert_after(a, c)?,
            None => {
                let root = d.root();
                d.append_child(root, c)?;
            }
        }
        c
    };
    fill_comment(doc, part, node, body, None)?;
    let key = key_para_id(doc, part, node)?.unwrap_or(0);
    add_extensions(doc, key, parent_para, author, date)?;
    Ok((part, node, key))
}

// ---------------------------------------------------------------------- positions

/// Where to insert a marker: before a node, or appended to a parent.
#[derive(Debug, Clone, Copy)]
enum Pos {
    Before(NodeId),
    Append(NodeId),
}

fn insert_at(d: &mut Document, pos: Pos, n: NodeId) -> Result<()> {
    match pos {
        Pos::Before(b) => d.insert_before(b, n),
        Pos::Append(p) => d.append_child(p, n),
    }
}

/// Text segments of a paragraph in the accepted view: (element, start byte, end byte).
fn segments(d: &Document, p: NodeId) -> (String, Vec<(NodeId, usize, usize)>) {
    let mut s = String::new();
    let mut v = Vec::new();
    text::visit(d, p, TextMode::AcceptAll, |n, t| {
        let start = s.len();
        s.push_str(t);
        v.push((n, start, s.len()));
    });
    (s, v)
}

/// Split the run holding `child` so `child` starts a run; returns the position before it.
fn split_before(d: &mut Document, wq: &Wq, child: NodeId, alloc: &mut dyn FnMut() -> u32) -> Result<Pos> {
    let Some(run) = d.parent(child).filter(|&r| is_w(d, r, "r")) else { return Ok(Pos::Before(child)) };
    let first_content = d.child_elements(run).find(|&c| !is_w(d, c, "rPr"));
    if first_content == Some(child) {
        return Ok(Pos::Before(run));
    }
    let r2 = wq.el(d, "r", &[])?;
    if let Some(rpr) = d.find_child(run, ns::W, "rPr") {
        let c = d.clone_subtree(rpr)?;
        // A tracked format change cloned with the run needs its own id.
        for ch in d.descendants(c).collect::<Vec<_>>() {
            if is_w(d, ch, "rPrChange") {
                d.set_attr(ch, &wq.q("id"), &alloc().to_string())?;
            }
        }
        d.append_child(r2, c)?;
    }
    let mut cur = Some(child);
    while let Some(c) = cur {
        cur = d.next_sibling(c);
        d.append_child(r2, c)?;
    }
    d.insert_after(run, r2)?;
    Ok(Pos::Before(r2))
}

/// Position after `child` in its run (splitting the run when something follows).
fn split_after(d: &mut Document, wq: &Wq, child: NodeId, alloc: &mut dyn FnMut() -> u32) -> Result<Pos> {
    let Some(run) = d.parent(child).filter(|&r| is_w(d, r, "r")) else {
        return Ok(match d.next_sibling(child) {
            Some(n) => Pos::Before(n),
            None => Pos::Append(d.parent(child).expect("parent")),
        });
    };
    let next = d.child_elements(run).skip_while(|&c| c != child).nth(1);
    match next {
        Some(next) => split_before(d, wq, next, alloc),
        None => Ok(match d.next_sibling(run) {
            Some(n) => Pos::Before(n),
            None => Pos::Append(d.parent(run).expect("parent")),
        }),
    }
}

/// Split a `w:t` at byte `at` of its text; returns the new second `w:t`.
fn split_t(d: &mut Document, wq: &Wq, t: NodeId, at: usize) -> Result<NodeId> {
    let s = d.text_content(t);
    let (a, b) = s.split_at(at);
    let (a, b) = (a.to_owned(), b.to_owned());
    let space = |x: &str| x.starts_with(char::is_whitespace) || x.ends_with(char::is_whitespace);
    d.set_text(t, &a)?;
    if space(&a) {
        d.set_attr(t, "xml:space", "preserve")?;
    }
    let t2 = wq.el(d, "t", &[])?;
    if space(&b) {
        d.set_attr(t2, "xml:space", "preserve")?;
    }
    let tx = d.create_text(&b)?;
    d.append_child(t2, tx)?;
    d.insert_after(t, t2)?;
    Ok(t2)
}

/// Position of the boundary at byte `off` in a paragraph's accepted text. `end` picks the
/// segment ending at `off` (a range end) rather than the one starting there.
fn boundary(
    d: &mut Document,
    wq: &Wq,
    segs: &[(NodeId, usize, usize)],
    off: usize,
    end: bool,
    alloc: &mut dyn FnMut() -> u32,
) -> Result<Pos> {
    let seg =
        if end { segs.iter().find(|s| s.1 < off && off <= s.2) } else { segs.iter().find(|s| s.1 <= off && off < s.2) };
    let &(n, start, stop) = seg.ok_or_else(|| err("NOT_FOUND", "range boundary outside the text"))?;
    // Paragraph separators are emitted on the w:p itself; never used for a single paragraph.
    if end && off == stop {
        return split_after(d, wq, n, alloc);
    }
    if !end && off == start {
        return split_before(d, wq, n, alloc);
    }
    if !is_w(d, n, "t") {
        return Err(err("UNSUPPORTED", "range boundary inside a non-text element"));
    }
    let t2 = split_t(d, wq, n, off - start)?;
    split_before(d, wq, t2, alloc)
}

/// After the outermost element between `n` and paragraph `p` (where a reference run may go).
fn after_outermost(d: &Document, n: NodeId, p: NodeId) -> Pos {
    let mut top = n;
    while let Some(par) = d.parent(top) {
        if par == p {
            break;
        }
        top = par;
    }
    match d.next_sibling(top) {
        Some(x) => Pos::Before(x),
        None => Pos::Append(d.parent(top).unwrap_or(p)),
    }
}

/// Position at the start of a paragraph's content (after its pPr).
fn para_start(d: &Document, p: NodeId) -> Pos {
    let first = d.children(p).find(|&c| !is_w(d, c, "pPr"));
    match first {
        Some(c) => Pos::Before(c),
        None => Pos::Append(p),
    }
}

// ---------------------------------------------------------------------- add / reply

/// `add_comment`.
pub fn add_comment(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    alloc_rev: &mut dyn FnMut() -> u32,
    author: &str,
    date: &str,
    args: &AddCommentArgs,
) -> Result<EditOutcome> {
    let author = args.author.as_deref().unwrap_or(author);
    let target =
        anchors.resolve(doc, &args.anchor).ok_or_else(|| err("NOT_FOUND", format!("no anchor {}", args.anchor)))?;
    // The paragraphs the comment may cover, and the story they are in.
    let (part, paras, drawing) = match target {
        Target::Block { story, para } => {
            let st = &doc.stories()[story];
            match st.kind {
                StoryKind::Body | StoryKind::Footnote { .. } | StoryKind::Endnote { .. } => {}
                _ => {
                    return Err(err(
                        "UNSUPPORTED",
                        "comments can be anchored in the body, footnotes and endnotes only",
                    ));
                }
            }
            (para.part, vec![para.node], None)
        }
        Target::Table { table: l } | Target::Row { row: l, .. } | Target::Cell { cell: l, .. } => {
            let d = doc.part(l.part);
            (l.part, d.descendants(l.node).filter(|&c| is_w(d, c, "p")).collect(), None)
        }
        Target::Image { drawing } => {
            let d = doc.part(drawing.part);
            let p = enclosing_p(d, drawing.node).ok_or_else(|| err("UNSUPPORTED", "image outside a paragraph"))?;
            (drawing.part, vec![p], Some(drawing.node))
        }
        Target::Comment { .. } => return Err(err("UNSUPPORTED", "comments on comments: use reply_comment")),
    };
    if paras.is_empty() {
        return Err(err("NOT_FOUND", "no paragraph to comment on"));
    }
    if part != doc.main_part() && !matches!(doc.part_kind(part), PartKind::Footnotes | PartKind::Endnotes) {
        return Err(err("UNSUPPORTED", "comments can be anchored in the body, footnotes and endnotes only"));
    }

    let mut tracker = Tracker::new(anchors);
    let id = fresh_comment_id(doc, alloc_rev);
    let ids = id.to_string();
    let styled = has_style(doc, "CommentReference");
    {
        let d = doc.part_mut(part);
        let wq = Wq::of(d);
        let start = wq.el(d, "commentRangeStart", &[("id", &ids)])?;
        let end = wq.el(d, "commentRangeEnd", &[("id", &ids)])?;
        let end_p;
        if let Some(find) = args.find.as_deref() {
            // Matches per paragraph, in order.
            let mut hits: Vec<(NodeId, usize)> = Vec::new();
            for &p in &paras {
                let (s, _) = segments(d, p);
                hits.extend(s.match_indices(find).map(|(i, _)| (p, i)));
            }
            let (p, at) = match (hits.len(), args.occurrence) {
                (0, _) => return Err(err("NOT_FOUND", format!("{find:?} not found in {}", args.anchor))),
                (_, Some(k)) if k as usize > hits.len() || k == 0 => {
                    return Err(err("NOT_FOUND", format!("occurrence {k} of {find:?}: only {} found", hits.len())));
                }
                (_, Some(k)) => hits[k as usize - 1],
                (1, None) => hits[0],
                (n, None) => {
                    return Err(err(
                        "AMBIGUOUS",
                        format!("{find:?} occurs {n} times in {}; give occurrence", args.anchor),
                    ));
                }
            };
            // End first: the start boundary's nodes stay put.
            let (_, segs) = segments(d, p);
            let epos = boundary(d, &wq, &segs, at + find.len(), true, alloc_rev)?;
            insert_at(d, epos, end)?;
            let (_, segs) = segments(d, p);
            let spos = boundary(d, &wq, &segs, at, false, alloc_rev)?;
            insert_at(d, spos, start)?;
            end_p = p;
        } else if let Some(drawing) = drawing {
            let run = d.ancestors(drawing).find(|&a| is_w(d, a, "r")).unwrap_or(drawing);
            d.insert_before(run, start)?;
            d.insert_after(run, end)?;
            end_p = paras[0];
        } else {
            let first = paras[0];
            let last = *paras.last().unwrap();
            let spos = para_start(d, first);
            insert_at(d, spos, start)?;
            d.append_child(last, end)?;
            end_p = last;
        }
        let r = reference_run(d, &wq, &ids, styled)?;
        let rpos = after_outermost(d, end, end_p);
        insert_at(d, rpos, r)?;
        for &p in &paras {
            tracker.touch(doc, part, p);
        }
    }
    let (cpart, cnode, _) = create_comment(doc, id, author, date, &args.text, None, None)?;
    let mut out = tracker.finish(doc, anchors, vec![id]);
    out.changed.retain(|h| !h.starts_with('c'));
    if let Some(h) = comment_handle(doc, anchors, cpart, cnode) {
        out.inserted.insert(0, h);
    }
    Ok(out)
}

/// The thread root of a comment (itself unless it is a reply), as (w:comment node, key paraId).
fn thread_root(doc: &WordDoc, part: PartIdx, node: NodeId) -> (NodeId, Option<u32>) {
    let d = doc.part(part);
    let Some(key) = read_key_para_id(d, node) else { return (node, None) };
    let parent = part_of(doc, PartKind::CommentsExtended).and_then(|ext| {
        let e = doc.part(ext);
        let entry = find_entry(e, ns::W15, "commentEx", "paraId", key)?;
        e.attr_ns(entry, ns::W15, "paraIdParent").and_then(|v| parse_hex_id(&v))
    });
    match parent {
        Some(pid) => {
            let root = d.child_elements(d.root()).find(|&c| read_key_para_id(d, c) == Some(pid));
            match root {
                Some(r) => (r, Some(pid)),
                None => (node, Some(key)),
            }
        }
        None => (node, Some(key)),
    }
}

/// Comments of a thread (root first) by key paraId.
fn thread_members(doc: &WordDoc, part: PartIdx, root_key: u32) -> Vec<NodeId> {
    let d = doc.part(part);
    let children: HashSet<u32> = part_of(doc, PartKind::CommentsExtended)
        .map(|ext| {
            let e = doc.part(ext);
            entries(e, ns::W15, "commentEx")
                .into_iter()
                .filter(|&c| e.attr_ns(c, ns::W15, "paraIdParent").and_then(|v| parse_hex_id(&v)) == Some(root_key))
                .filter_map(|c| e.attr_ns(c, ns::W15, "paraId").and_then(|v| parse_hex_id(&v)))
                .collect()
        })
        .unwrap_or_default();
    d.child_elements(d.root())
        .filter(|&c| read_key_para_id(d, c).is_some_and(|k| k == root_key || children.contains(&k)))
        .collect()
}

/// `reply_comment`.
pub fn reply_comment(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    alloc_rev: &mut dyn FnMut() -> u32,
    author: &str,
    date: &str,
    args: &ReplyCommentArgs,
) -> Result<EditOutcome> {
    let author = args.author.as_deref().unwrap_or(author);
    let target = comment_by_anchor(doc, anchors, &args.comment)?;
    let (root, _) = thread_root(doc, target.part, target.node);
    let root_id = w_attr(doc.part(target.part), root, "id").unwrap_or_default();
    let root_key = key_para_id(doc, target.part, root)?.ok_or_else(|| err("INVALID_OP", "comment has no paragraph"))?;
    let after = thread_members(doc, target.part, root_key).last().copied().unwrap_or(root);

    let mut tracker = Tracker::new(anchors);
    let id = fresh_comment_id(doc, alloc_rev);
    let ids = id.to_string();
    let styled = has_style(doc, "CommentReference");
    // Duplicate the root's range markers and reference next to the originals.
    for part in story_parts(doc) {
        let d = doc.part(part);
        let found: Vec<NodeId> = d
            .descendants(d.document_node())
            .filter(|&n| {
                matches!(w_local(d, n), Some("commentRangeStart" | "commentRangeEnd" | "commentReference"))
                    && w_attr(d, n, "id").as_deref() == Some(root_id.as_str())
            })
            .collect();
        if found.is_empty() {
            continue;
        }
        for &n in &found {
            tracker.touch(doc, part, n);
        }
        let d = doc.part_mut(part);
        let wq = Wq::of(d);
        for n in found {
            let local = w_local(d, n).unwrap_or("").to_owned();
            if local == "commentReference" {
                let run = d.parent(n).filter(|&r| is_w(d, r, "r")).unwrap_or(n);
                let r = reference_run(d, &wq, &ids, styled)?;
                d.insert_after(run, r)?;
            } else {
                let m = wq.el(d, &local, &[("id", &ids)])?;
                d.insert_after(n, m)?;
            }
        }
    }
    let (cpart, cnode, _) = create_comment(doc, id, author, date, &args.text, Some(after), Some(root_key))?;
    let mut out = tracker.finish(doc, anchors, vec![id]);
    out.changed.retain(|h| !h.starts_with('c'));
    if let Some(h) = comment_handle(doc, anchors, cpart, cnode) {
        out.inserted.insert(0, h);
    }
    Ok(out)
}

// ---------------------------------------------------------------------- resolve / edit / delete

/// `resolve_comment`: set `w15:done` (creating the commentEx entry when missing).
pub fn resolve_comment(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    _alloc_rev: &mut dyn FnMut() -> u32,
    _author: &str,
    _date: &str,
    args: &ResolveCommentArgs,
) -> Result<EditOutcome> {
    let target = comment_by_anchor(doc, anchors, &args.comment)?;
    let key =
        key_para_id(doc, target.part, target.node)?.ok_or_else(|| err("INVALID_OP", "comment has no paragraph"))?;
    let ext = ensure_part(doc, PartKind::CommentsExtended)?;
    let d = doc.part_mut(ext);
    let p15 = prefix(d, ns::W15, "w15")?;
    let val = if args.done { "1" } else { "0" };
    match find_entry(d, ns::W15, "commentEx", "paraId", key) {
        Some(e) => d.set_attr(e, &q(&p15, "done"), val)?,
        None => {
            let e = d.create_element(&q(&p15, "commentEx"))?;
            d.set_attr(e, &q(&p15, "paraId"), &format!("{key:08X}"))?;
            d.set_attr(e, &q(&p15, "done"), val)?;
            let root = d.root();
            d.append_child(root, e)?;
        }
    }
    anchors.sync(doc);
    Ok(EditOutcome { changed: vec![args.comment.clone()], ..Default::default() })
}

/// `edit_comment`: replace the text, keeping the first paragraph's properties, the
/// annotationRef run and the key paraId (so threading and done state survive).
pub fn edit_comment(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    _alloc_rev: &mut dyn FnMut() -> u32,
    _author: &str,
    _date: &str,
    args: &EditCommentArgs,
) -> Result<EditOutcome> {
    let target = comment_by_anchor(doc, anchors, &args.comment)?;
    let (part, node) = (target.part, target.node);
    let key = key_para_id(doc, part, node)?;
    let mut tracker = Tracker::new(anchors);
    let keep_ppr = {
        let d = doc.part_mut(part);
        let paras: Vec<NodeId> = d.child_elements(node).filter(|&c| is_w(d, c, "p")).collect();
        let ppr = paras.first().and_then(|&p| d.find_child(p, ns::W, "pPr"));
        let keep = match ppr {
            Some(p) => {
                d.detach(p)?;
                Some(p)
            }
            None => None,
        };
        for c in kids(d, node) {
            d.remove(c)?;
        }
        keep
    };
    fill_comment(doc, part, node, &args.text, keep_ppr)?;
    // The key paraId moves to the new last paragraph.
    if let Some(key) = key {
        let d = doc.part_mut(part);
        let paras: Vec<NodeId> = d.child_elements(node).filter(|&c| is_w(d, c, "p")).collect();
        let last = *paras.last().expect("filled");
        let p14 = prefix(d, ns::W14, "w14")?;
        d.set_attr(last, &q(&p14, "paraId"), &format!("{key:08X}"))?;
    }
    tracker.touch_all_paragraphs(doc, part, node);
    let mut out = tracker.finish(doc, anchors, Vec::new());
    out.changed.insert(0, args.comment.clone());
    Ok(out)
}

/// `delete_comment`: the comment and its replies, their ranges and references, and their
/// entries in the extension parts.
pub fn delete_comment(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    _alloc_rev: &mut dyn FnMut() -> u32,
    _author: &str,
    _date: &str,
    args: &DeleteCommentArgs,
) -> Result<EditOutcome> {
    let target = comment_by_anchor(doc, anchors, &args.comment)?;
    let part = target.part;
    let mut doomed: Vec<NodeId> = vec![target.node];
    // Replies, transitively.
    let mut frontier: Vec<u32> = read_key_para_id(doc.part(part), target.node).into_iter().collect();
    let mut seen: HashSet<u32> = frontier.iter().copied().collect();
    while let Some(k) = frontier.pop() {
        for m in thread_members(doc, part, k) {
            if let Some(mk) = read_key_para_id(doc.part(part), m)
                && seen.insert(mk)
            {
                doomed.push(m);
                frontier.push(mk);
            }
        }
    }
    let ids: HashSet<String> = doomed.iter().filter_map(|&c| w_attr(doc.part(part), c, "id")).collect();
    let keys: HashSet<u32> = doomed.iter().filter_map(|&c| read_key_para_id(doc.part(part), c)).collect();
    let handles: Vec<String> = doomed.iter().filter_map(|&c| comment_handle(doc, anchors, part, c)).collect();

    let mut tracker = Tracker::new(anchors);
    // Markers in the stories.
    for sp in story_parts(doc) {
        let d = doc.part(sp);
        let found: Vec<NodeId> = d
            .descendants(d.document_node())
            .filter(|&n| {
                matches!(w_local(d, n), Some("commentRangeStart" | "commentRangeEnd" | "commentReference"))
                    && w_attr(d, n, "id").is_some_and(|v| ids.contains(&v))
            })
            .collect();
        for &n in &found {
            tracker.touch(doc, sp, n);
        }
        let d = doc.part_mut(sp);
        for n in found {
            if !attached(d, n) {
                continue;
            }
            let run = d.parent(n).filter(|&r| is_w(d, r, "r"));
            match run {
                Some(r) if d.child_elements(r).all(|c| c == n || is_w(d, c, "rPr")) => d.remove(r)?,
                _ => d.remove(n)?,
            }
        }
    }
    // Comment elements.
    {
        let d = doc.part_mut(part);
        for c in &doomed {
            if attached(d, *c) {
                d.remove(*c)?;
            }
        }
    }
    // Extension entries.
    let mut durables: HashSet<u32> = HashSet::new();
    if let Some(p) = part_of(doc, PartKind::CommentsIds) {
        let d = doc.part_mut(p);
        for e in entries(d, ns::W16CID, "commentId") {
            let pid = d.attr_ns(e, ns::W16CID, "paraId").and_then(|v| parse_hex_id(&v));
            if pid.is_some_and(|k| keys.contains(&k)) {
                if let Some(dur) = d.attr_ns(e, ns::W16CID, "durableId").and_then(|v| parse_hex_id(&v)) {
                    durables.insert(dur);
                }
                d.remove(e)?;
            }
        }
    }
    if let Some(p) = part_of(doc, PartKind::CommentsExtended) {
        let d = doc.part_mut(p);
        for e in entries(d, ns::W15, "commentEx") {
            if d.attr_ns(e, ns::W15, "paraId").and_then(|v| parse_hex_id(&v)).is_some_and(|k| keys.contains(&k)) {
                d.remove(e)?;
            }
        }
    }
    if let Some(p) = part_of(doc, PartKind::CommentsExtensible) {
        let d = doc.part_mut(p);
        for e in entries(d, ns::W16CEX, "commentExtensible") {
            if d.attr_ns(e, ns::W16CEX, "durableId")
                .and_then(|v| parse_hex_id(&v))
                .is_some_and(|k| durables.contains(&k))
            {
                d.remove(e)?;
            }
        }
    }
    let mut out = tracker.finish(doc, anchors, Vec::new());
    out.changed.retain(|h| !h.starts_with('c'));
    out.deleted.retain(|h| !h.starts_with('c'));
    for h in handles.into_iter().rev() {
        out.deleted.insert(0, h);
    }
    Ok(out)
}

// ---------------------------------------------------------------------- listing

/// `list_comments`: threads in comment order, replies nested under their root.
pub fn list_comments(doc: &WordDoc, anchors: &Anchors, args: &ListCommentsArgs) -> Result<CommentList> {
    // Extension data by key paraId.
    let mut done: HashMap<u32, bool> = HashMap::new();
    let mut parent_of: HashMap<u32, u32> = HashMap::new();
    if let Some(ext) = part_of(doc, PartKind::CommentsExtended) {
        let e = doc.part(ext);
        for c in entries(e, ns::W15, "commentEx") {
            let Some(pid) = e.attr_ns(c, ns::W15, "paraId").and_then(|v| parse_hex_id(&v)) else { continue };
            let dn = e.attr_ns(c, ns::W15, "done").is_some_and(|v| v.trim() == "1" || v.trim() == "true");
            done.insert(pid, dn);
            if let Some(par) = e.attr_ns(c, ns::W15, "paraIdParent").and_then(|v| parse_hex_id(&v)) {
                parent_of.insert(pid, par);
            }
        }
    }

    // Range text and anchor per comment w:id, from the stories.
    let mut quoted: HashMap<String, String> = HashMap::new();
    let mut anchor_of: HashMap<String, String> = HashMap::new();
    for sp in story_parts(doc) {
        let d = doc.part(sp);
        let mut starts: HashMap<String, NodeId> = HashMap::new();
        let mut ends: HashMap<String, NodeId> = HashMap::new();
        let mut refs: HashMap<String, NodeId> = HashMap::new();
        for n in d.descendants(d.document_node()) {
            let m = match w_local(d, n) {
                Some("commentRangeStart") => &mut starts,
                Some("commentRangeEnd") => &mut ends,
                Some("commentReference") => &mut refs,
                _ => continue,
            };
            if let Some(id) = w_attr(d, n, "id") {
                m.entry(id).or_insert(n);
            }
        }
        if starts.is_empty() && refs.is_empty() {
            continue;
        }
        let rank = order_index(d);
        let mut segs: Vec<(usize, String)> = Vec::new();
        text::visit(d, d.root(), TextMode::AcceptAll, |n, t| segs.push((rank[&n], t.to_owned())));
        for (id, &s) in &starts {
            if let Some(&e) = ends.get(id) {
                let (a, b) = (rank[&s], rank[&e]);
                let q: String = segs.iter().filter(|(r, _)| *r > a && *r < b).map(|(_, t)| t.as_str()).collect();
                quoted.insert(id.clone(), q);
            }
        }
        for (id, &n) in starts.iter().chain(refs.iter()) {
            if anchor_of.contains_key(id) {
                continue;
            }
            if let Some(h) = enclosing_p(d, n).and_then(|p| anchors.handle_of(Loc::new(sp, p))) {
                anchor_of.insert(id.clone(), h.to_owned());
            }
        }
    }

    let mut all: Vec<(Option<u32>, CommentInfo)> = Vec::new();
    for (si, st) in doc.stories().iter().enumerate() {
        if !matches!(st.kind, StoryKind::Comment { .. }) {
            continue;
        }
        let d = doc.part(st.part);
        let c = st.root;
        let wid = w_attr(d, c, "id").unwrap_or_default();
        let key = read_key_para_id(d, c);
        all.push((
            key,
            CommentInfo {
                id: anchors.story_id(si).unwrap_or("c").to_owned(),
                author: w_attr(d, c, "author").unwrap_or_default(),
                initials: w_attr(d, c, "initials"),
                date: w_attr(d, c, "date"),
                done: key.and_then(|k| done.get(&k).copied()).unwrap_or(false),
                on: quoted.get(&wid).cloned().unwrap_or_default(),
                anchor: anchor_of.get(&wid).cloned(),
                text: text::text(d, c, TextMode::AcceptAll),
                replies: Vec::new(),
            },
        ));
    }
    // Nest replies under their root.
    let keys: HashMap<u32, usize> = all.iter().enumerate().filter_map(|(i, (k, _))| k.map(|k| (k, i))).collect();
    let mut roots: Vec<usize> = Vec::new();
    let mut replies: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, (k, _)) in all.iter().enumerate() {
        let mut parent = k.and_then(|k| parent_of.get(&k)).and_then(|p| keys.get(p)).copied();
        // Climb to the thread root.
        let mut guard = 0;
        while let Some(p) = parent {
            let up = all[p].0.and_then(|k| parent_of.get(&k)).and_then(|pp| keys.get(pp)).copied();
            match up {
                Some(u) if u != i && guard < 64 => {
                    parent = Some(u);
                    guard += 1;
                }
                _ => break,
            }
        }
        match parent.filter(|&p| p != i) {
            Some(p) => replies.entry(p).or_default().push(i),
            None => roots.push(i),
        }
    }
    let mut slots: Vec<Option<CommentInfo>> = all.into_iter().map(|(_, c)| Some(c)).collect();
    let mut out = Vec::new();
    for r in roots {
        let mut c = slots[r].take().expect("root once");
        if args.open_only == Some(true) && c.done {
            continue;
        }
        for i in replies.remove(&r).unwrap_or_default() {
            if let Some(rep) = slots[i].take() {
                c.replies.push(rep);
            }
        }
        out.push(c);
    }
    Ok(CommentList { comments: out })
}

#[cfg(test)]
mod tests {
    use super::super::id_allocator;
    use super::super::test_util::{body_xml, doc};
    use super::*;

    fn add(wd: &mut WordDoc, an: &mut Anchors, anchor: &str, find: Option<&str>, text: &str) -> Result<EditOutcome> {
        let mut alloc = id_allocator(wd);
        let args = AddCommentArgs {
            anchor: anchor.into(),
            find: find.map(Into::into),
            text: text.into(),
            ..Default::default()
        };
        add_comment(wd, an, &mut alloc, "Ann Lee", "2026-01-01T00:00:00Z", &args)
    }

    #[test]
    fn add_on_found_text_splits_minimally() {
        let mut wd = doc(r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Hello brave world</w:t></w:r></w:p>"#);
        let mut an = Anchors::mint(&wd);
        let h = an.handles(0).next().unwrap().0.to_owned();
        let out = add(&mut wd, &mut an, &h, Some("brave"), "Really?").unwrap();
        assert_eq!(out.inserted[0], "c1");
        assert_eq!(out.changed, vec![h.clone()]);
        let x = body_xml(&wd);
        assert!(
            x.starts_with(r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">Hello </w:t></w:r><w:commentRangeStart w:id="0"/><w:r><w:rPr><w:b/></w:rPr><w:t>brave</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve"> world</w:t></w:r></w:p>"#),
            "{x}"
        );
        for k in [
            PartKind::Comments,
            PartKind::CommentsExtended,
            PartKind::CommentsIds,
            PartKind::CommentsExtensible,
            PartKind::People,
        ] {
            assert!(part_of(&wd, k).is_some(), "{k:?}");
        }
        let l = list_comments(&wd, &an, &ListCommentsArgs::default()).unwrap();
        assert_eq!(l.comments.len(), 1);
        let c = &l.comments[0];
        assert_eq!(
            (c.id.as_str(), c.on.as_str(), c.text.as_str(), c.author.as_str()),
            ("c1", "brave", "Really?", "Ann Lee")
        );
        assert_eq!(c.initials.as_deref(), Some("AL"));
        assert_eq!(c.anchor.as_deref(), Some(h.as_str()));
    }

    #[test]
    fn ambiguity_and_missing() {
        let mut wd = doc(r#"<w:p><w:r><w:t>a b a</w:t></w:r></w:p>"#);
        let mut an = Anchors::mint(&wd);
        let h = an.handles(0).next().unwrap().0.to_owned();
        assert!(add(&mut wd, &mut an, &h, Some("a"), "x").unwrap_err().to_string().starts_with("AMBIGUOUS"));
        assert!(add(&mut wd, &mut an, &h, Some("zz"), "x").unwrap_err().to_string().starts_with("NOT_FOUND"));
        let mut alloc = id_allocator(&wd);
        let args = AddCommentArgs {
            anchor: h,
            find: Some("a".into()),
            occurrence: Some(2),
            text: "x".into(),
            ..Default::default()
        };
        add_comment(&mut wd, &mut an, &mut alloc, "A", "d", &args).unwrap();
        let l = list_comments(&wd, &an, &ListCommentsArgs::default()).unwrap();
        assert_eq!(l.comments[0].on, "a");
        assert!(body_xml(&wd).contains(r#"<w:t xml:space="preserve">a b </w:t></w:r><w:commentRangeStart"#));
    }

    #[test]
    fn reply_resolve_edit_delete() {
        let mut wd = doc(r#"<w:p><w:r><w:t>one</w:t></w:r></w:p><w:p><w:r><w:t>two</w:t></w:r></w:p>"#);
        let mut an = Anchors::mint(&wd);
        let h = an.handles(0).next().unwrap().0.to_owned();
        add(&mut wd, &mut an, &h, None, "first").unwrap();
        let mut alloc = id_allocator(&wd);
        let r = ReplyCommentArgs {
            comment: "c1".into(),
            text: "answer".into(),
            author: Some("Bo".into()),
            ..Default::default()
        };
        let out = reply_comment(&mut wd, &mut an, &mut alloc, "X", "d", &r).unwrap();
        assert_eq!(out.inserted[0], "c2");
        let rs = ResolveCommentArgs { comment: "c1".into(), done: true, ..Default::default() };
        resolve_comment(&mut wd, &mut an, &mut alloc, "X", "d", &rs).unwrap();
        let e = EditCommentArgs { comment: "c1".into(), text: "first, edited\nline two".into(), ..Default::default() };
        edit_comment(&mut wd, &mut an, &mut alloc, "X", "d", &e).unwrap();
        let l = list_comments(&wd, &an, &ListCommentsArgs::default()).unwrap();
        assert_eq!(l.comments.len(), 1);
        let c = &l.comments[0];
        assert!(c.done);
        assert_eq!(c.on, "one");
        assert_eq!(c.text, "first, edited\nline two");
        assert_eq!(c.replies.len(), 1);
        assert_eq!(c.replies[0].text, "answer");
        assert!(
            list_comments(&wd, &an, &ListCommentsArgs { open_only: Some(true), ..Default::default() })
                .unwrap()
                .comments
                .is_empty()
        );

        let del = DeleteCommentArgs { comment: "c1".into(), ..Default::default() };
        let out = delete_comment(&mut wd, &mut an, &mut alloc, "X", "d", &del).unwrap();
        assert!(out.deleted.contains(&"c1".to_owned()) && out.deleted.contains(&"c2".to_owned()), "{out:?}");
        assert_eq!(body_xml(&wd), r#"<w:p><w:r><w:t>one</w:t></w:r></w:p><w:p><w:r><w:t>two</w:t></w:r></w:p>"#);
        assert!(list_comments(&wd, &an, &ListCommentsArgs::default()).unwrap().comments.is_empty());
        let ext = part_of(&wd, PartKind::CommentsExtended).unwrap();
        assert!(entries(wd.part(ext), ns::W15, "commentEx").is_empty());
    }
}
