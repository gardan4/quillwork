//! Stories (DESIGN §4.2): the independent text flows of a document.
//!
//! Order is fixed and deterministic: body; headers, then footers (by first reference from a
//! section in document order, unreferenced ones last by part name); footnotes, endnotes (by first
//! reference in the main part, unreferenced ones in part order; separators skipped); comments (by
//! first range start / reference across body, headers, footers and notes, the rest in part
//! order); text boxes (discovered by scanning the stories in this order, nested ones after their
//! host). The glossary document is preserved only and never becomes a story.

use std::collections::{HashMap, HashSet};

use super::doc::{PartIdx, PartKind, WordDoc};
use super::tree::{Step, is_ignored_branch, is_w, w_attr, walk};
use crate::xml::{Document, NodeId, ns};

/// Which header/footer of a section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HfType {
    Default,
    First,
    Even,
}

impl HfType {
    fn parse(v: Option<&str>) -> HfType {
        match v {
            Some("first") => HfType::First,
            Some("even") => HfType::Even,
            _ => HfType::Default,
        }
    }
}

/// One use of a header/footer part by a section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HfRef {
    /// Index into [`WordDoc::sections`].
    pub section: usize,
    pub hf: HfType,
}

/// A section: one `w:sectPr` of the body, with its header/footer references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub sect_pr: NodeId,
    pub headers: Vec<(HfType, PartIdx)>,
    pub footers: Vec<(HfType, PartIdx)>,
}

/// What kind of story, with its identifying data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoryKind {
    Body,
    /// A header part; `refs` empty when no section references it.
    Header {
        refs: Vec<HfRef>,
    },
    Footer {
        refs: Vec<HfRef>,
    },
    /// `w:footnote/@w:id`.
    Footnote {
        id: i64,
    },
    Endnote {
        id: i64,
    },
    /// `w:comment/@w:id`.
    Comment {
        id: i64,
    },
    /// A `w:txbxContent`. `host` is the index of the story it sits in. When it is the canonical
    /// `mc:Choice` copy, `fallback` is its VML twin in the `mc:Fallback` (kept in sync by writers).
    /// `drawing_id` is the enclosing `wp:docPr/@id` when there is one.
    TextBox {
        host: usize,
        fallback: Option<NodeId>,
        drawing_id: Option<u32>,
    },
}

/// A story: its kind, the part it lives in and its root element (`w:body`, `w:hdr`, `w:ftr`,
/// `w:footnote`, `w:endnote`, `w:comment` or `w:txbxContent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Story {
    pub kind: StoryKind,
    pub part: PartIdx,
    pub root: NodeId,
}

impl Story {
    /// The anchor prefix of this story kind: `b`, `h`, `f`, `fn`, `en`, `c`, `tb`.
    pub fn prefix(&self) -> &'static str {
        match self.kind {
            StoryKind::Body => "b",
            StoryKind::Header { .. } => "h",
            StoryKind::Footer { .. } => "f",
            StoryKind::Footnote { .. } => "fn",
            StoryKind::Endnote { .. } => "en",
            StoryKind::Comment { .. } => "c",
            StoryKind::TextBox { .. } => "tb",
        }
    }
}

const SEPARATOR_TYPES: &[&str] = &["separator", "continuationSeparator", "continuationNotice"];

pub(crate) fn discover(doc: &WordDoc) -> (Vec<Story>, Vec<Section>) {
    let main = doc.main_part();
    let md = doc.part(main);
    let body = md.find_child(md.root(), ns::W, "body").unwrap_or(md.root());
    let mut stories = vec![Story { kind: StoryKind::Body, part: main, root: body }];

    // Sections and header/footer references.
    let sections = sections(doc, md, body);
    let mut hf_refs: Vec<(PartIdx, HfRef)> = Vec::new();
    let mut hf_order: Vec<PartIdx> = Vec::new();
    for (si, s) in sections.iter().enumerate() {
        for &(hf, p) in s.headers.iter().chain(&s.footers) {
            hf_refs.push((p, HfRef { section: si, hf }));
            if !hf_order.contains(&p) {
                hf_order.push(p);
            }
        }
    }
    for kind in [PartKind::Header, PartKind::Footer] {
        let mut parts: Vec<PartIdx> = hf_order.iter().copied().filter(|&p| doc.part_kind(p) == kind).collect();
        let mut rest: Vec<PartIdx> = doc.parts_of(kind).filter(|p| !parts.contains(p)).collect();
        rest.sort_by(|a, b| doc.part_name(*a).cmp(doc.part_name(*b)));
        parts.extend(rest);
        for p in parts {
            let refs = hf_refs.iter().filter(|(q, _)| *q == p).map(|(_, r)| *r).collect();
            let root = doc.part(p).root();
            let kind = if kind == PartKind::Header { StoryKind::Header { refs } } else { StoryKind::Footer { refs } };
            stories.push(Story { kind, part: p, root });
        }
    }

    // Notes, ordered by first reference in the main part.
    for (part, elem, reference) in
        [(doc.footnotes_part(), "footnote", "footnoteReference"), (doc.endnotes_part(), "endnote", "endnoteReference")]
    {
        let Some(part) = part else { continue };
        let refs = ids_in_order(md, md.root(), &[reference]);
        let pd = doc.part(part);
        let notes: Vec<(i64, NodeId)> = pd
            .child_elements(pd.root())
            .filter(|&n| is_w(pd, n, elem))
            .filter_map(|n| {
                let ty = w_attr(pd, n, "type");
                if ty.as_deref().is_some_and(|t| SEPARATOR_TYPES.contains(&t)) {
                    return None;
                }
                let id = w_attr(pd, n, "id").and_then(|v| v.trim().parse::<i64>().ok())?;
                if (id == -1 || id == 0) && !refs.contains(&id) {
                    return None;
                }
                Some((id, n))
            })
            .collect();
        for (id, n) in order_by_refs(&notes, &refs) {
            let kind = if elem == "footnote" { StoryKind::Footnote { id } } else { StoryKind::Endnote { id } };
            stories.push(Story { kind, part, root: n });
        }
    }

    // Comments, ordered by first range start / reference across the stories so far.
    if let Some(part) = doc.comments_part() {
        let mut refs = Vec::new();
        for s in &stories {
            for id in ids_in_order(doc.part(s.part), s.root, &["commentRangeStart", "commentReference"]) {
                if !refs.contains(&id) {
                    refs.push(id);
                }
            }
        }
        let pd = doc.part(part);
        let comments: Vec<(i64, NodeId)> = pd
            .child_elements(pd.root())
            .filter(|&n| is_w(pd, n, "comment"))
            .filter_map(|n| Some((w_attr(pd, n, "id")?.trim().parse::<i64>().ok()?, n)))
            .collect();
        for (id, n) in order_by_refs(&comments, &refs) {
            stories.push(Story { kind: StoryKind::Comment { id }, part, root: n });
        }
    }

    // Text boxes: scan every story (including text boxes found on the way) in order.
    let mut i = 0;
    while i < stories.len() {
        let (part, root) = (stories[i].part, stories[i].root);
        let d = doc.part(part);
        let mut found = Vec::new();
        walk(d, root, |n| {
            if is_ignored_branch(d, n) {
                return Step::Over;
            }
            if is_w(d, n, "txbxContent") {
                found.push(n);
                return Step::Over; // nested boxes are found when this box is scanned
            }
            Step::Into
        });
        for tb in found {
            stories.push(Story {
                kind: StoryKind::TextBox { host: i, fallback: fallback_twin(d, tb), drawing_id: drawing_id(d, tb) },
                part,
                root: tb,
            });
        }
        i += 1;
    }
    (stories, sections)
}

fn sections(doc: &WordDoc, md: &Document, body: NodeId) -> Vec<Section> {
    let main = doc.main_part();
    let mut out = Vec::new();
    walk(md, body, |n| {
        if is_ignored_branch(md, n)
            || is_w(md, n, "txbxContent")
            || is_w(md, n, "sectPrChange")
            || is_w(md, n, "pPrChange")
        {
            return Step::Over;
        }
        if is_w(md, n, "sectPr") {
            let parent = md.parent(n);
            let ok = parent == Some(body)
                || parent.is_some_and(|p| is_w(md, p, "pPr") && md.parent(p).is_some_and(|pp| is_w(md, pp, "p")));
            if ok {
                let mut s = Section { sect_pr: n, headers: Vec::new(), footers: Vec::new() };
                for c in md.child_elements(n) {
                    let header = is_w(md, c, "headerReference");
                    if !header && !is_w(md, c, "footerReference") {
                        continue;
                    }
                    let hf = HfType::parse(w_attr(md, c, "type").as_deref());
                    let Some(rid) = md.attr_ns(c, ns::R, "id") else { continue };
                    let Some(name) = doc.rel_target_part(main, &rid) else { continue };
                    let Some(p) = doc.part_idx(&name) else { continue };
                    if header { s.headers.push((hf, p)) } else { s.footers.push((hf, p)) }
                }
                out.push(s);
            }
            return Step::Over;
        }
        Step::Into
    });
    out
}

/// `@w:id` of every element named one of `locals` below `root`, first occurrence order.
fn ids_in_order(d: &Document, root: NodeId, locals: &[&str]) -> Vec<i64> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for n in d.descendants(root) {
        if d.namespace(n) != Some(ns::W) || !d.local_name(n).is_some_and(|l| locals.contains(&l)) {
            continue;
        }
        if let Some(id) = w_attr(d, n, "id").and_then(|v| v.trim().parse::<i64>().ok())
            && seen.insert(id)
        {
            out.push(id);
        }
    }
    out
}

/// Referenced items in reference order, then the unreferenced ones in part order. Duplicate ids
/// in the part keep part order after the first.
fn order_by_refs(items: &[(i64, NodeId)], refs: &[i64]) -> Vec<(i64, NodeId)> {
    let mut first: HashMap<i64, usize> = HashMap::new();
    for (i, (id, _)) in items.iter().enumerate() {
        first.entry(*id).or_insert(i);
    }
    let mut used = vec![false; items.len()];
    let mut out = Vec::with_capacity(items.len());
    for id in refs {
        if let Some(&i) = first.get(id)
            && !used[i]
        {
            used[i] = true;
            out.push(items[i]);
        }
    }
    for (i, it) in items.iter().enumerate() {
        if !used[i] {
            out.push(*it);
        }
    }
    out
}

/// For a `w:txbxContent` in an `mc:Choice`: the matching `w:txbxContent` (same ordinal) in the
/// `mc:Fallback` of the nearest `mc:AlternateContent`.
fn fallback_twin(d: &Document, tb: NodeId) -> Option<NodeId> {
    let choice = d.ancestors(tb).find(|&a| d.is(a, ns::MC, "Choice"))?;
    let ac = d.parent(choice)?;
    let fallback = d.find_child(ac, ns::MC, "Fallback")?;
    let k = d.descendants_named(choice, ns::W, "txbxContent").position(|n| n == tb)?;
    d.descendants_named(fallback, ns::W, "txbxContent").nth(k)
}

/// `wp:docPr/@id` of the drawing that holds `n`.
pub(crate) fn drawing_id(d: &Document, n: NodeId) -> Option<u32> {
    let holder = d.ancestors(n).find(|&a| d.is(a, ns::WP, "inline") || d.is(a, ns::WP, "anchor"))?;
    let pr = d.find_child(holder, ns::WP, "docPr")?;
    d.attr(pr, "id")?.trim().parse().ok()
}
