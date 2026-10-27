//! Tracked changes: list them, accept or reject them (DESIGN §7.4).
//!
//! Accept and reject are structural, per revision type:
//!
//! | revision                         | accept                       | reject                         |
//! |----------------------------------|------------------------------|--------------------------------|
//! | `ins` / `moveTo` (runs)          | unwrap                       | remove (range markup kept)     |
//! | `del` / `moveFrom` (runs)        | remove (range markup kept)   | unwrap, `delText` → `t`        |
//! | paragraph mark inserted          | drop the marker              | merge with the next paragraph  |
//! | paragraph mark deleted           | merge with the next paragraph| drop the marker                |
//! | `*PrChange`                      | drop the change              | restore the old properties     |
//! | `tblGridChange`                  | drop the change              | restore the old grid           |
//! | row inserted (`trPr/ins`)        | drop the marker              | remove the row                 |
//! | row deleted (`trPr/del`)         | remove the row               | drop the marker                |
//! | `cellIns` / `cellDel`            | drop marker / remove cell    | remove cell / drop marker      |
//! | `cellMerge`, `numberingChange`   | drop the marker              | drop the marker (not restored) |
//!
//! A move is resolved as a whole: selecting either half selects both (by move range name) and
//! the `move*RangeStart/End` markers go with it. Revisions are applied in reverse document
//! order, so nested revisions (an insertion later deleted) resolve inside-out.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{
    EditOutcome, Guards, Tracker, Wq, attached, enclosing_p, err, has_content, is_ignored_branch, is_w, kids,
    undelete_text, unwrap, w_attr, w_local, walk,
};
use crate::anchors::Anchors;
use crate::error::Result;
use crate::wml::view::{Row, Table};
use crate::wml::{Loc, PartIdx, StoryKind, TextMode, WordDoc, text};
use crate::xml::{Document, NodeId, ns};

// ---------------------------------------------------------------------- args and results

/// `list_changes`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListChangesArgs {
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub spec_version: Option<String>,
    #[serde(default)]
    pub change_set: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    /// The revision ids of `change_set`, filled by the engine from the host sidecar (the
    /// document itself does not record change sets).
    #[serde(skip)]
    pub change_set_revisions: Option<Vec<u32>>,
}

/// `accept` / `reject`. Exactly one selector.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReviewArgs {
    #[serde(flatten)]
    pub guards: Guards,
    #[serde(default)]
    pub change_set: Option<String>,
    #[serde(default)]
    pub revision_ids: Option<Vec<u64>>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub all: Option<bool>,
    /// See [`ListChangesArgs::change_set_revisions`].
    #[serde(skip)]
    pub change_set_revisions: Option<Vec<u32>>,
}

/// One revision, as `list_changes` reports it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RevisionInfo {
    pub id: i64,
    /// `ins`, `del`, `moveFrom`, `moveTo`, `paraMarkIns`, `paraMarkDel`, `fmt` (run properties),
    /// `pfmt` (paragraph properties), `sectFmt`, `tableFmt`, `tableExFmt`, `rowFmt`, `cellFmt`,
    /// `rowIns`, `rowDel`, `cellIns`, `cellDel`, `cellMerge`, `numbering`, `gridChange`.
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// Story id (`body`, `h1`, `fn2`, `c1`, …).
    pub story: String,
    pub part: String,
    /// Block handles the revision sits in or covers; row / cell / table anchors for table
    /// revisions.
    pub anchors: Vec<String>,
    /// As-is text of a content revision (empty for marks and property changes).
    pub text: String,
    /// `open` (the only status knowable from the document alone).
    pub status: &'static str,
    /// The move range name pairing a `moveFrom` with its `moveTo`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_name: Option<String>,
}

/// `list_changes` result.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ChangeList {
    pub revisions: Vec<RevisionInfo>,
}

// ---------------------------------------------------------------------- classification

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum K {
    Ins,
    Del,
    MoveFrom,
    MoveTo,
    MarkIns,
    MarkDel,
    MarkMoveFrom,
    MarkMoveTo,
    /// `w:r/w:rPr/w:ins|del` (run-mark markers) and `w:numPr/w:ins`: not listed, dropped on resolve.
    Marker,
    RowIns,
    RowDel,
    CellIns,
    CellDel,
    CellMerge,
    /// A `*PrChange`.
    Prop,
    Grid,
    Numbering,
}

fn classify(d: &Document, n: NodeId) -> Option<K> {
    let local = w_local(d, n)?;
    let parent = d.parent(n);
    let parent_local = parent.and_then(|p| w_local(d, p));
    let in_mark = parent_local == Some("rPr") && parent.and_then(|p| d.parent(p)).is_some_and(|pp| is_w(d, pp, "pPr"));
    Some(match local {
        "ins" | "del" | "moveFrom" | "moveTo" => match parent_local {
            Some("trPr") => match local {
                "ins" => K::RowIns,
                "del" => K::RowDel,
                _ => K::Marker,
            },
            Some("rPr") if in_mark => match local {
                "ins" => K::MarkIns,
                "del" => K::MarkDel,
                "moveFrom" => K::MarkMoveFrom,
                _ => K::MarkMoveTo,
            },
            Some("rPr" | "numPr") => K::Marker,
            _ => match local {
                "ins" => K::Ins,
                "del" => K::Del,
                "moveFrom" => K::MoveFrom,
                _ => K::MoveTo,
            },
        },
        "cellIns" => K::CellIns,
        "cellDel" => K::CellDel,
        "cellMerge" => K::CellMerge,
        "rPrChange" | "pPrChange" | "sectPrChange" | "tblPrChange" | "tblPrExChange" | "trPrChange" | "tcPrChange" => {
            K::Prop
        }
        "tblGridChange" => K::Grid,
        "numberingChange" => K::Numbering,
        _ => return None,
    })
}

fn kind_name(d: &Document, n: NodeId, k: K) -> &'static str {
    match k {
        K::Ins => "ins",
        K::Del => "del",
        K::MoveFrom | K::MarkMoveFrom => "moveFrom",
        K::MoveTo | K::MarkMoveTo => "moveTo",
        K::MarkIns => "paraMarkIns",
        K::MarkDel => "paraMarkDel",
        K::Marker => "marker",
        K::RowIns => "rowIns",
        K::RowDel => "rowDel",
        K::CellIns => "cellIns",
        K::CellDel => "cellDel",
        K::CellMerge => "cellMerge",
        K::Grid => "gridChange",
        K::Numbering => "numbering",
        K::Prop => match w_local(d, n) {
            Some("rPrChange") => "fmt",
            Some("pPrChange") => "pfmt",
            Some("sectPrChange") => "sectFmt",
            Some("tblPrChange") => "tableFmt",
            Some("tblPrExChange") => "tableExFmt",
            Some("trPrChange") => "rowFmt",
            _ => "cellFmt",
        },
    }
}

fn is_move(k: K) -> bool {
    matches!(k, K::MoveFrom | K::MoveTo | K::MarkMoveFrom | K::MarkMoveTo)
}

/// Does the walk stop at this revision (its inside is a property snapshot)?
fn opaque(k: K) -> bool {
    matches!(k, K::Prop | K::Grid | K::Numbering)
}

#[derive(Debug, Clone)]
struct Item {
    part: PartIdx,
    node: NodeId,
    k: K,
    id: Option<i64>,
    author: Option<String>,
    move_name: Option<String>,
}

/// Open move ranges while walking, by kind (`from` / `to`): range id → name.
#[derive(Default)]
struct MoveRanges {
    open: Vec<(bool, String, String)>,
}

impl MoveRanges {
    fn see(&mut self, d: &Document, n: NodeId) {
        match w_local(d, n) {
            Some(l @ ("moveFromRangeStart" | "moveToRangeStart")) => {
                let from = l == "moveFromRangeStart";
                let id = w_attr(d, n, "id").unwrap_or_default();
                let name = w_attr(d, n, "name").unwrap_or_default();
                self.open.push((from, id, name));
            }
            Some(l @ ("moveFromRangeEnd" | "moveToRangeEnd")) => {
                let from = l == "moveFromRangeEnd";
                let id = w_attr(d, n, "id").unwrap_or_default();
                if let Some(i) = self.open.iter().rposition(|(f, i, _)| *f == from && *i == id) {
                    self.open.remove(i);
                }
            }
            _ => {}
        }
    }
    fn name(&self, from: bool) -> Option<String> {
        self.open.iter().rev().find(|(f, _, _)| *f == from).map(|(_, _, n)| n.clone()).filter(|n| !n.is_empty())
    }
}

/// Revisions below `root` in document order. `whole_part` walks everything (alternate content
/// branches and text boxes included) as accept / reject must; otherwise the walk follows the
/// story's own content only, as a listing does.
fn collect(d: &Document, part: PartIdx, root: NodeId, whole_part: bool, out: &mut Vec<Item>) {
    let mut moves = MoveRanges::default();
    walk(d, root, |n| {
        if !whole_part && (is_ignored_branch(d, n) || is_w(d, n, "txbxContent")) {
            return false;
        }
        moves.see(d, n);
        let Some(k) = classify(d, n) else { return true };
        let move_name = match k {
            K::MoveFrom | K::MarkMoveFrom => moves.name(true),
            K::MoveTo | K::MarkMoveTo => moves.name(false),
            _ => None,
        };
        out.push(Item {
            part,
            node: n,
            k,
            id: w_attr(d, n, "id").and_then(|v| v.trim().parse().ok()),
            author: w_attr(d, n, "author"),
            move_name,
        });
        !opaque(k)
    });
}

// ---------------------------------------------------------------------- listing

/// Positional anchors (`tN.rM`, `tN.rM.cK`) for rows and cells.
pub(crate) struct TablePositions {
    tables: HashMap<Loc, String>,
}

impl TablePositions {
    pub(crate) fn new(anchors: &Anchors) -> Self {
        TablePositions { tables: anchors.table_anchors().map(|(h, l)| (l, h.to_owned())).collect() }
    }

    pub(crate) fn table(&self, doc: &WordDoc, part: PartIdx, n: NodeId) -> Option<(String, NodeId)> {
        let d = doc.part(part);
        let tbl = if is_w(d, n, "tbl") { n } else { d.ancestors(n).find(|&a| is_w(d, a, "tbl"))? };
        Some((self.tables.get(&Loc::new(part, tbl))?.clone(), tbl))
    }

    pub(crate) fn row(&self, doc: &WordDoc, part: PartIdx, n: NodeId) -> Option<String> {
        let d = doc.part(part);
        let tr = if is_w(d, n, "tr") { n } else { d.ancestors(n).find(|&a| is_w(d, a, "tr"))? };
        let (t, tbl) = self.table(doc, part, tr)?;
        let i = Table(tbl).rows(d).iter().position(|r| r.0 == tr)?;
        Some(format!("{t}.r{}", i + 1))
    }

    pub(crate) fn cell(&self, doc: &WordDoc, part: PartIdx, n: NodeId) -> Option<String> {
        let d = doc.part(part);
        let tc = if is_w(d, n, "tc") { n } else { d.ancestors(n).find(|&a| is_w(d, a, "tc"))? };
        let tr = d.ancestors(tc).find(|&a| is_w(d, a, "tr"))?;
        let row = self.row(doc, part, tr)?;
        let i = Row(tr).cells(d).iter().position(|c| c.0 == tc)?;
        Some(format!("{row}.c{}", i + 1))
    }
}

/// `list_changes`: every open revision across all stories, filtered.
pub fn list_changes(doc: &WordDoc, anchors: &Anchors, args: &ListChangesArgs) -> Result<ChangeList> {
    if args.status.as_deref().is_some_and(|s| s != "open") {
        return Ok(ChangeList::default());
    }
    let set: Option<HashSet<i64>> = match (&args.change_set, &args.change_set_revisions) {
        (None, _) => None,
        (Some(_), Some(ids)) => Some(ids.iter().map(|&i| i as i64).collect()),
        (Some(cs), None) => {
            return Err(err("INVALID_OP", format!("change set {cs} is not known to this document")));
        }
    };
    let pos = TablePositions::new(anchors);
    let mut out = Vec::new();
    for (si, story) in doc.stories().iter().enumerate() {
        let d = doc.part(story.part);
        let mut items = Vec::new();
        collect(d, story.part, story.root, false, &mut items);
        let story_id = anchors.story_id(si).unwrap_or(story.prefix()).to_owned();
        for it in items {
            if it.k == K::Marker {
                continue;
            }
            if args.author.as_ref().is_some_and(|a| it.author.as_ref() != Some(a)) {
                continue;
            }
            if set.as_ref().is_some_and(|s| !it.id.is_some_and(|id| s.contains(&id))) {
                continue;
            }
            let content = matches!(it.k, K::Ins | K::Del | K::MoveFrom | K::MoveTo);
            out.push(RevisionInfo {
                id: it.id.unwrap_or(-1),
                kind: kind_name(d, it.node, it.k),
                author: it.author.clone(),
                date: w_attr(d, it.node, "date"),
                story: story_id.clone(),
                part: doc.part_name(story.part).to_owned(),
                anchors: revision_anchors(doc, anchors, &pos, &it),
                text: if content {
                    text::text(d, it.node, TextMode::AsIs)
                } else if matches!(it.k, K::RowIns | K::RowDel) {
                    d.ancestors(it.node)
                        .find(|&a| is_w(d, a, "tr"))
                        .map(|tr| text::text(d, tr, TextMode::AsIs))
                        .unwrap_or_default()
                } else {
                    String::new()
                },
                status: "open",
                move_name: it.move_name.clone(),
            });
        }
    }
    Ok(ChangeList { revisions: out })
}

fn revision_anchors(doc: &WordDoc, anchors: &Anchors, pos: &TablePositions, it: &Item) -> Vec<String> {
    let d = doc.part(it.part);
    let mut out: Vec<String> = Vec::new();
    let local = w_local(d, it.node).unwrap_or("");
    match it.k {
        K::RowIns | K::RowDel => out.extend(pos.row(doc, it.part, it.node)),
        K::CellIns | K::CellDel | K::CellMerge => out.extend(pos.cell(doc, it.part, it.node)),
        K::Prop if local == "trPrChange" || local == "tblPrExChange" => out.extend(pos.row(doc, it.part, it.node)),
        K::Prop if local == "tcPrChange" => out.extend(pos.cell(doc, it.part, it.node)),
        K::Prop if local == "tblPrChange" => out.extend(pos.table(doc, it.part, it.node).map(|t| t.0)),
        K::Grid => out.extend(pos.table(doc, it.part, it.node).map(|t| t.0)),
        _ => {}
    }
    let mut push = |p: NodeId| {
        if let Some(h) = anchors.handle_of(Loc::new(it.part, p))
            && !out.iter().any(|x| x == h)
        {
            out.push(h.to_owned());
        }
    };
    if let Some(p) = enclosing_p(d, it.node) {
        push(p);
    }
    if matches!(it.k, K::RowIns | K::RowDel)
        && let Some(tr) = d.ancestors(it.node).find(|&a| is_w(d, a, "tr"))
    {
        for p in d.descendants(tr).filter(|&p| is_w(d, p, "p")) {
            push(p);
        }
    }
    out
}

// ---------------------------------------------------------------------- accept / reject

/// Accept revisions.
pub fn accept(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    _alloc_rev: &mut dyn FnMut() -> u32,
    _author: &str,
    _date: &str,
    args: &ReviewArgs,
) -> Result<EditOutcome> {
    apply(doc, anchors, args, true)
}

/// Reject revisions.
pub fn reject(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    _alloc_rev: &mut dyn FnMut() -> u32,
    _author: &str,
    _date: &str,
    args: &ReviewArgs,
) -> Result<EditOutcome> {
    apply(doc, anchors, args, false)
}

/// Accept (`accept = true`) or reject the selected revisions. `EditOutcome::revisions` lists
/// the ids resolved.
pub fn apply(doc: &mut WordDoc, anchors: &mut Anchors, args: &ReviewArgs, accept: bool) -> Result<EditOutcome> {
    let selectors =
        [args.change_set.is_some(), args.revision_ids.is_some(), args.author.is_some(), args.all == Some(true)];
    if selectors.iter().filter(|&&s| s).count() != 1 {
        return Err(err("INVALID_OP", "give exactly one of change_set, revision_ids, author, all"));
    }

    // Every revision in every story part.
    let mut parts: Vec<PartIdx> = Vec::new();
    for s in doc.stories() {
        if !parts.contains(&s.part) && !matches!(s.kind, StoryKind::TextBox { .. }) {
            parts.push(s.part);
        }
    }
    let mut items = Vec::new();
    for &p in &parts {
        let d = doc.part(p);
        collect(d, p, d.document_node(), true, &mut items);
    }

    // Selection.
    let mut selected: Vec<bool> = match (&args.revision_ids, &args.change_set_revisions, &args.author) {
        _ if args.all == Some(true) => vec![true; items.len()],
        (Some(ids), _, _) => {
            let want: HashSet<i64> = ids.iter().map(|&i| i as i64).collect();
            let found: HashSet<i64> = items.iter().filter_map(|it| it.id).filter(|id| want.contains(id)).collect();
            let mut missing: Vec<i64> = want.difference(&found).copied().collect();
            if !missing.is_empty() {
                missing.sort();
                return Err(err("NOT_FOUND", format!("no revision with id {missing:?}")));
            }
            items.iter().map(|it| it.id.is_some_and(|id| want.contains(&id))).collect()
        }
        (None, Some(cs), _) if args.change_set.is_some() => {
            let want: HashSet<i64> = cs.iter().map(|&i| i as i64).collect();
            let sel: Vec<bool> = items.iter().map(|it| it.id.is_some_and(|id| want.contains(&id))).collect();
            if !sel.iter().any(|&s| s) {
                return Err(err("NOT_FOUND", "the change set has no open revisions"));
            }
            sel
        }
        (None, None, _) if args.change_set.is_some() => {
            return Err(err(
                "INVALID_OP",
                format!("change set {} is not known to this document", args.change_set.as_deref().unwrap_or("")),
            ));
        }
        (_, _, Some(a)) => {
            let sel: Vec<bool> = items.iter().map(|it| it.author.as_deref() == Some(a.as_str())).collect();
            if !sel.iter().any(|&s| s) {
                return Err(err("NOT_FOUND", format!("no revisions by {a:?}")));
            }
            sel
        }
        _ => unreachable!(),
    };

    // A move resolves as a whole.
    let moves: HashSet<(PartIdx, String)> = items
        .iter()
        .zip(&selected)
        .filter(|(it, s)| **s && is_move(it.k))
        .filter_map(|(it, _)| it.move_name.clone().map(|n| (it.part, n)))
        .collect();
    // Move names are document-wide in practice (both halves in one story part).
    let move_names: HashSet<&str> = moves.iter().map(|(_, n)| n.as_str()).collect();
    for (it, s) in items.iter().zip(selected.iter_mut()) {
        if is_move(it.k) && it.move_name.as_deref().is_some_and(|n| move_names.contains(n)) {
            *s = true;
        }
    }

    let mut tracker = Tracker::new(anchors);
    let mut resolved: Vec<u32> = Vec::new();
    for (it, _) in items.iter().zip(&selected).filter(|(_, s)| **s).rev().collect::<Vec<_>>() {
        if !attached(doc.part(it.part), it.node) {
            continue;
        }
        if let Some(id) = it.id.and_then(|i| u32::try_from(i).ok())
            && !resolved.contains(&id)
        {
            resolved.push(id);
        }
        tracker.touch(doc, it.part, it.node);
        let d = doc.part_mut(it.part);
        resolve_one(d, it, accept)?;
    }
    // Move range markers of resolved moves.
    if !move_names.is_empty() {
        for &p in &parts {
            let d = doc.part_mut(p);
            remove_move_ranges(d, &move_names)?;
        }
    }
    resolved.sort_unstable();
    Ok(tracker.finish(doc, anchors, resolved))
}

fn resolve_one(d: &mut Document, it: &Item, accept: bool) -> Result<()> {
    let wq = Wq::of(d);
    let n = it.node;
    match (it.k, accept) {
        (K::Ins | K::MoveTo, true) => unwrap(d, n),
        (K::Del | K::MoveFrom, false) => {
            undelete_text(d, &wq, n)?;
            unwrap(d, n)
        }
        (K::Ins | K::MoveTo, false) | (K::Del | K::MoveFrom, true) => remove_keeping_markup(d, n),
        (K::MarkIns | K::MarkMoveTo, true) | (K::MarkDel | K::MarkMoveFrom, false) => drop_marker(d, n),
        (K::MarkIns | K::MarkMoveTo, false) | (K::MarkDel | K::MarkMoveFrom, true) => {
            let p = enclosing_p(d, n);
            drop_marker(d, n)?;
            match p {
                Some(p) => merge_with_next(d, p),
                None => Ok(()),
            }
        }
        (K::Marker | K::CellMerge | K::Numbering, _) => drop_marker(d, n),
        (K::RowIns, true) | (K::RowDel, false) => drop_marker(d, n),
        (K::RowIns, false) | (K::RowDel, true) => match ancestor(d, n, "tr") {
            Some(tr) => remove_row(d, &wq, tr),
            None => drop_marker(d, n),
        },
        (K::CellIns, true) | (K::CellDel, false) => drop_marker(d, n),
        (K::CellIns, false) | (K::CellDel, true) => match ancestor(d, n, "tc") {
            Some(tc) => {
                let at = super::tables::cell_grid_start(d, tc);
                d.remove(tc)?;
                match at {
                    Some((tbl, start)) => super::tables::shrink_grid_after_cell_removal(d, tbl, start),
                    None => Ok(()),
                }
            }
            None => drop_marker(d, n),
        },
        (K::Prop | K::Grid, true) => drop_marker(d, n),
        (K::Prop, false) => restore_props(d, n),
        (K::Grid, false) => restore_grid(d, n),
    }
}

fn ancestor(d: &Document, n: NodeId, local: &str) -> Option<NodeId> {
    d.ancestors(n).find(|&a| is_w(d, a, local))
}

/// Elements that mark a range, not content: kept when the content around them goes.
pub(crate) fn is_range_markup(d: &Document, n: NodeId) -> bool {
    matches!(
        w_local(d, n),
        Some(
            "bookmarkStart"
                | "bookmarkEnd"
                | "commentRangeStart"
                | "commentRangeEnd"
                | "moveFromRangeStart"
                | "moveFromRangeEnd"
                | "moveToRangeStart"
                | "moveToRangeEnd"
                | "permStart"
                | "permEnd"
                | "customXmlInsRangeStart"
                | "customXmlInsRangeEnd"
                | "customXmlDelRangeStart"
                | "customXmlDelRangeEnd"
                | "customXmlMoveFromRangeStart"
                | "customXmlMoveFromRangeEnd"
                | "customXmlMoveToRangeStart"
                | "customXmlMoveToRangeEnd"
        )
    )
}

/// Remove `n`, first moving out (before it) the range markup it holds and any comment
/// reference runs, so bookmarks and comments stay paired.
pub(crate) fn remove_keeping_markup(d: &mut Document, n: NodeId) -> Result<()> {
    let keep: Vec<NodeId> = d
        .descendants(n)
        .filter(|&c| c != n)
        .filter(|&c| {
            is_range_markup(d, c) || (is_w(d, c, "r") && d.child_elements(c).any(|x| is_w(d, x, "commentReference")))
        })
        .collect();
    let mut moved: Vec<NodeId> = Vec::new();
    for c in keep {
        // Skip what is already inside a kept run.
        if moved.iter().any(|&m| d.ancestors(c).any(|a| a == m)) {
            continue;
        }
        if is_w(d, c, "r") {
            for x in kids(d, c) {
                if !matches!(w_local(d, x), Some("rPr" | "commentReference")) {
                    d.remove(x)?;
                }
            }
        }
        d.insert_before(n, c)?;
        moved.push(c);
    }
    d.remove(n)
}

/// Remove a marker element; then its parent if that is an emptied `rPr` / `trPr` / `tblPrEx`.
fn drop_marker(d: &mut Document, n: NodeId) -> Result<()> {
    let parent = d.parent(n);
    d.remove(n)?;
    if let Some(p) = parent {
        prune_empty(d, p)?;
    }
    Ok(())
}

fn prune_empty(d: &mut Document, n: NodeId) -> Result<()> {
    if matches!(w_local(d, n), Some("rPr" | "trPr" | "tblPrEx" | "pPr" | "numPr"))
        && d.child_elements(n).next().is_none()
    {
        let parent = d.parent(n);
        // A paragraph mark's rPr can go; the pPr it leaves may go too.
        d.remove(n)?;
        if let Some(p) = parent.filter(|&p| is_w(d, p, "pPr"))
            && d.child_elements(p).next().is_none()
            && d.attr_count(p) == 0
        {
            d.remove(p)?;
        }
    }
    Ok(())
}

/// Join paragraph `p` (whose mark is gone) with the next paragraph.
///
/// An empty `p` simply disappears (its range markup moves into the next paragraph). Otherwise
/// the next paragraph's content is appended to `p`; `p` keeps its paragraph properties, takes
/// the next paragraph's mark run properties and section break, and the next paragraph goes.
/// Before a table or a block content control nothing joins: the paragraph stays, as in Word.
/// A rejected (or accepted-deleted) paragraph mark with no paragraph after it: an empty body
/// paragraph goes when the block before it is a paragraph, or a table that is itself a tracked
/// insertion (e.g. the paragraph `insert_table` adds after a table at the end of the body).
fn drop_trailing_empty(d: &mut Document, p: NodeId) -> Result<()> {
    if has_content(d, p) || d.descendants(p).any(|c| is_range_markup(d, c) || is_w(d, c, "sectPr")) {
        return Ok(());
    }
    if !d.parent(p).is_some_and(|b| is_w(d, b, "body")) {
        return Ok(());
    }
    let mut prev = d.prev_sibling(p);
    while let Some(x) = prev.filter(|&x| !d.is_element_node(x)) {
        prev = d.prev_sibling(x);
    }
    let ok = prev.is_some_and(|x| {
        is_w(d, x, "p")
            || (is_w(d, x, "tbl")
                && d.child_elements(x)
                    .filter(|&r| is_w(d, r, "tr"))
                    .all(|r| d.find_child(r, ns::W, "trPr").and_then(|t| d.find_child(t, ns::W, "ins")).is_some()))
    });
    if ok { d.remove(p) } else { Ok(()) }
}

fn merge_with_next(d: &mut Document, p: NodeId) -> Result<()> {
    let mut next = d.next_sibling(p);
    let mut between: Vec<NodeId> = Vec::new();
    while let Some(x) = next {
        if d.is_element_node(x) {
            if is_range_markup(d, x) || is_w(d, x, "proofErr") {
                between.push(x);
            } else {
                break;
            }
        }
        next = d.next_sibling(x);
    }
    let Some(nx) = next.filter(|&x| is_w(d, x, "p")) else {
        // Nothing joins a table or a block content control: Word keeps the paragraph.
        if next.is_some_and(|x| matches!(w_local(d, x), Some("tbl" | "sdt" | "customXml"))) {
            return Ok(());
        }
        return drop_trailing_empty(d, p);
    };
    let p_ppr = d.find_child(p, ns::W, "pPr");
    let n_ppr = d.find_child(nx, ns::W, "pPr");

    if !has_content(d, p) {
        // Move p's markup into nx (after its pPr), drop p.
        let mut anchor = n_ppr;
        for c in kids(d, p) {
            if Some(c) == p_ppr {
                continue;
            }
            let empty_run = is_w(d, c, "r") && d.child_elements(c).all(|x| is_w(d, x, "rPr"));
            let empty_wrap = matches!(w_local(d, c), Some("ins" | "del" | "moveFrom" | "moveTo"))
                && d.child_elements(c).next().is_none();
            if empty_run || empty_wrap || is_w(d, c, "proofErr") {
                continue;
            }
            match anchor {
                Some(a) => d.insert_after(a, c)?,
                None => d.prepend_child(nx, c)?,
            }
            anchor = Some(c);
        }
        return d.remove(p);
    }

    // p absorbs nx.
    for b in between {
        d.append_child(p, b)?;
    }
    for c in kids(d, nx) {
        if Some(c) != n_ppr {
            d.append_child(p, c)?;
        }
    }
    let wq = Wq::of(d);
    if let Some(np) = n_ppr {
        let ppr = match p_ppr {
            Some(pp) => pp,
            None => {
                let pp = wq.el(d, "pPr", &[])?;
                d.prepend_child(p, pp)?;
                pp
            }
        };
        // Mark run properties and section break come from the surviving mark (nx's).
        for local in ["rPr", "sectPr"] {
            if let Some(old) = d.find_child(ppr, ns::W, local) {
                d.remove(old)?;
            }
        }
        for local in ["rPr", "sectPr"] {
            if let Some(c) = d.find_child(np, ns::W, local) {
                match d.find_child(ppr, ns::W, "pPrChange") {
                    Some(ch) => d.insert_before(ch, c)?,
                    None => d.append_child(ppr, c)?,
                }
            }
        }
        if d.child_elements(ppr).next().is_none() && d.attr_count(ppr) == 0 {
            d.remove(ppr)?;
        }
    } else if let Some(pp) = p_ppr {
        for local in ["rPr", "sectPr"] {
            if let Some(old) = d.find_child(pp, ns::W, local) {
                d.remove(old)?;
            }
        }
        if d.child_elements(pp).next().is_none() && d.attr_count(pp) == 0 {
            d.remove(pp)?;
        }
    }
    d.remove(nx)
}

/// Remove a row; an emptied table goes too (and a cell it leaves without a paragraph gets one).
pub(crate) fn remove_row(d: &mut Document, wq: &Wq, tr: NodeId) -> Result<()> {
    let tbl = d.ancestors(tr).find(|&a| is_w(d, a, "tbl"));
    d.remove(tr)?;
    if let Some(tbl) = tbl
        && !d.descendants(tbl).any(|c| is_w(d, c, "tr"))
    {
        let container = d.parent(tbl);
        d.remove(tbl)?;
        if let Some(tc) = container.filter(|&c| is_w(d, c, "tc")) {
            let last = d.child_elements(tc).last();
            if !last.is_some_and(|l| is_w(d, l, "p")) {
                let p = wq.el(d, "p", &[])?;
                d.append_child(tc, p)?;
            }
        }
    }
    Ok(())
}

/// Reject a `*PrChange`: the parent properties element gets the snapshot's children back,
/// keeping what the snapshot does not cover (mark revision markers, `rPr` / `sectPr` of a
/// `pPr`, header / footer references, row and cell revision markers).
fn restore_props(d: &mut Document, change: NodeId) -> Result<()> {
    let Some(parent) = d.parent(change) else { return d.remove(change) };
    let snapshot = d.child_elements(change).next();
    let plocal = w_local(d, parent).unwrap_or("").to_owned();
    let (keep_before, keep_after): (&[&str], &[&str]) = match plocal.as_str() {
        "rPr" => (&["ins", "del", "moveFrom", "moveTo"], &[]),
        "pPr" => (&[], &["rPr", "sectPr"]),
        "sectPr" => (&["headerReference", "footerReference"], &[]),
        "trPr" => (&[], &["ins", "del"]),
        "tcPr" => (&[], &["cellIns", "cellDel", "cellMerge"]),
        _ => (&[], &[]),
    };
    let mut before = Vec::new();
    let mut after = Vec::new();
    for c in kids(d, parent) {
        if c == change {
            continue;
        }
        let l = w_local(d, c).unwrap_or("");
        if keep_before.contains(&l) {
            before.push(c);
        } else if keep_after.contains(&l) {
            after.push(c);
        } else {
            d.remove(c)?;
        }
    }
    // Rebuild: before…, snapshot children…, after….
    let restored: Vec<NodeId> = snapshot.map(|s| kids(d, s)).unwrap_or_default();
    d.remove(change)?;
    for c in &before {
        d.append_child(parent, *c)?;
    }
    for c in restored {
        d.append_child(parent, c)?;
    }
    for c in &after {
        d.append_child(parent, *c)?;
    }
    prune_empty(d, parent)
}

/// Reject a `tblGridChange`: the old `gridCol`s come back.
fn restore_grid(d: &mut Document, change: NodeId) -> Result<()> {
    let Some(grid) = d.parent(change) else { return d.remove(change) };
    let old: Vec<NodeId> = d
        .child_elements(change)
        .next()
        .map(|s| d.child_elements(s).filter(|&c| is_w(d, c, "gridCol")).collect())
        .unwrap_or_default();
    for c in kids(d, grid) {
        if is_w(d, c, "gridCol") {
            d.remove(c)?;
        }
    }
    let first = d.first_child(grid);
    for c in old {
        match first {
            Some(f) if f != change => d.insert_before(f, c)?,
            _ => d.insert_before(change, c)?,
        }
    }
    d.remove(change)
}

fn remove_move_ranges(d: &mut Document, names: &HashSet<&str>) -> Result<()> {
    let root = d.document_node();
    let mut ids: HashSet<(bool, String)> = HashSet::new();
    let mut doomed = Vec::new();
    for n in d.descendants(root) {
        if let Some(l @ ("moveFromRangeStart" | "moveToRangeStart")) = w_local(d, n)
            && w_attr(d, n, "name").is_some_and(|nm| names.contains(nm.as_str()))
        {
            ids.insert((l == "moveFromRangeStart", w_attr(d, n, "id").unwrap_or_default()));
            doomed.push(n);
        }
    }
    for n in d.descendants(root) {
        if let Some(l @ ("moveFromRangeEnd" | "moveToRangeEnd")) = w_local(d, n)
            && ids.contains(&(l == "moveFromRangeEnd", w_attr(d, n, "id").unwrap_or_default()))
        {
            doomed.push(n);
        }
    }
    for n in doomed {
        if attached(d, n) {
            d.remove(n)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_util::{body_xml, doc};
    use super::*;

    fn run(body: &str, accept_all: bool) -> (String, EditOutcome) {
        let mut wd = doc(body);
        let mut an = Anchors::mint(&wd);
        let args = ReviewArgs { all: Some(true), ..Default::default() };
        let out = apply(&mut wd, &mut an, &args, accept_all).unwrap();
        (body_xml(&wd), out)
    }

    const INS_DEL: &str = r#"<w:p><w:r><w:t>A</w:t></w:r><w:del w:id="1" w:author="X" w:date="2020-01-01T00:00:00Z"><w:r><w:delText>old</w:delText></w:r></w:del><w:ins w:id="2" w:author="X" w:date="2020-01-01T00:00:00Z"><w:r><w:t>new</w:t></w:r></w:ins></w:p>"#;

    #[test]
    fn accept_and_reject_runs() {
        let (x, out) = run(INS_DEL, true);
        assert_eq!(x, "<w:p><w:r><w:t>A</w:t></w:r><w:r><w:t>new</w:t></w:r></w:p>");
        assert_eq!(out.revisions, vec![1, 2]);
        let (x, _) = run(INS_DEL, false);
        assert_eq!(x, "<w:p><w:r><w:t>A</w:t></w:r><w:r><w:t>old</w:t></w:r></w:p>");
    }

    #[test]
    fn by_id_and_untouched_bytes() {
        let body = format!("<w:p><w:r><w:t xml:space=\"preserve\">keep  me</w:t></w:r></w:p>{INS_DEL}");
        let mut wd = doc(&body);
        let mut an = Anchors::mint(&wd);
        let args = ReviewArgs { revision_ids: Some(vec![2]), ..Default::default() };
        apply(&mut wd, &mut an, &args, true).unwrap();
        let x = body_xml(&wd);
        assert!(x.starts_with("<w:p><w:r><w:t xml:space=\"preserve\">keep  me</w:t></w:r></w:p>"));
        assert!(x.contains("<w:del w:id=\"1\""));
        assert!(!x.contains("<w:ins"));
        let args = ReviewArgs { revision_ids: Some(vec![99]), ..Default::default() };
        let e = apply(&mut wd, &mut an, &args, true).unwrap_err();
        assert!(e.to_string().starts_with("NOT_FOUND"));
        let e = apply(&mut wd, &mut an, &ReviewArgs::default(), true).unwrap_err();
        assert!(e.to_string().starts_with("INVALID_OP"));
    }

    #[test]
    fn nested_insert_then_delete() {
        let body = r#"<w:p><w:ins w:id="1" w:author="A" w:date="d"><w:del w:id="2" w:author="B" w:date="d"><w:r><w:delText>x</w:delText></w:r></w:del><w:r><w:t>y</w:t></w:r></w:ins></w:p>"#;
        let (x, _) = run(body, true);
        assert_eq!(x, "<w:p><w:r><w:t>y</w:t></w:r></w:p>");
        let (x, _) = run(body, false);
        assert_eq!(x, "<w:p></w:p>");
    }

    #[test]
    fn paragraph_mark_merge() {
        let body = r#"<w:p><w:pPr><w:jc w:val="center"/><w:rPr><w:del w:id="1" w:author="A" w:date="d"/></w:rPr></w:pPr><w:r><w:t>one </w:t></w:r></w:p><w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:t>two</w:t></w:r></w:p>"#;
        let (x, out) = run(body, true);
        assert_eq!(
            x,
            r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t>one </w:t></w:r><w:r><w:t>two</w:t></w:r></w:p>"#
        );
        assert_eq!(out.deleted.len(), 1);
        assert_eq!(out.changed.len(), 1);
        let (x, _) = run(body, false);
        assert!(x.starts_with(r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r>"#), "{x}");
    }

    #[test]
    fn inserted_paragraph_rejected() {
        let body = r#"<w:p><w:pPr><w:rPr><w:ins w:id="1" w:author="A" w:date="d"/></w:rPr></w:pPr><w:bookmarkStart w:id="0" w:name="b"/><w:ins w:id="2" w:author="A" w:date="d"><w:r><w:t>new</w:t></w:r></w:ins><w:bookmarkEnd w:id="0"/></w:p><w:p><w:r><w:t>old</w:t></w:r></w:p>"#;
        let (x, _) = run(body, false);
        assert_eq!(
            x,
            r#"<w:p><w:bookmarkStart w:id="0" w:name="b"/><w:bookmarkEnd w:id="0"/><w:r><w:t>old</w:t></w:r></w:p>"#
        );
        let (x, _) = run(body, true);
        assert!(x.starts_with(r#"<w:p><w:bookmarkStart"#), "{x}");
    }

    #[test]
    fn format_reject_restores() {
        let body = r#"<w:p><w:pPr><w:jc w:val="center"/><w:rPr><w:b/></w:rPr><w:pPrChange w:id="5" w:author="A" w:date="d"><w:pPr><w:ind w:left="720"/></w:pPr></w:pPrChange></w:pPr><w:r><w:rPr><w:i/><w:rPrChange w:id="6" w:author="A" w:date="d"><w:rPr><w:u w:val="single"/></w:rPr></w:rPrChange></w:rPr><w:t>x</w:t></w:r></w:p>"#;
        let (x, _) = run(body, false);
        assert_eq!(
            x,
            r#"<w:p><w:pPr><w:ind w:left="720"/><w:rPr><w:b/></w:rPr></w:pPr><w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t>x</w:t></w:r></w:p>"#
        );
        let (x, _) = run(body, true);
        assert_eq!(
            x,
            r#"<w:p><w:pPr><w:jc w:val="center"/><w:rPr><w:b/></w:rPr></w:pPr><w:r><w:rPr><w:i/></w:rPr><w:t>x</w:t></w:r></w:p>"#
        );
    }

    #[test]
    fn moves_resolve_both_halves() {
        let body = r#"<w:p><w:moveFromRangeStart w:id="10" w:author="A" w:date="d" w:name="m1"/><w:moveFrom w:id="1" w:author="A" w:date="d"><w:r><w:t>M</w:t></w:r></w:moveFrom><w:moveFromRangeEnd w:id="10"/></w:p><w:p><w:moveToRangeStart w:id="11" w:author="A" w:date="d" w:name="m1"/><w:moveTo w:id="2" w:author="A" w:date="d"><w:r><w:t>M</w:t></w:r></w:moveTo><w:moveToRangeEnd w:id="11"/></w:p>"#;
        let mut wd = doc(body);
        let mut an = Anchors::mint(&wd);
        let args = ReviewArgs { revision_ids: Some(vec![2]), ..Default::default() };
        let out = apply(&mut wd, &mut an, &args, true).unwrap();
        assert_eq!(out.revisions, vec![1, 2]);
        assert_eq!(body_xml(&wd), "<w:p></w:p><w:p><w:r><w:t>M</w:t></w:r></w:p>");
    }

    #[test]
    fn rows() {
        let body = r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="100"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:trPr><w:ins w:id="1" w:author="A" w:date="d"/></w:trPr><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>"#;
        let (x, _) = run(body, false);
        assert!(!x.contains(">b<") && x.contains(">a<"), "{x}");
        let (x, _) = run(body, true);
        assert!(x.contains(">b<") && !x.contains("trPr"), "{x}");
    }

    #[test]
    fn listing() {
        let body = format!(
            "{INS_DEL}<w:p><w:pPr><w:rPr><w:ins w:id=\"7\" w:author=\"Y\" w:date=\"d\"/></w:rPr></w:pPr></w:p>"
        );
        let wd = doc(&body);
        let an = Anchors::mint(&wd);
        let l = list_changes(&wd, &an, &ListChangesArgs::default()).unwrap();
        let kinds: Vec<_> = l.revisions.iter().map(|r| (r.id, r.kind, r.text.as_str())).collect();
        assert_eq!(kinds, vec![(1, "del", "old"), (2, "ins", "new"), (7, "paraMarkIns", "")]);
        assert_eq!(l.revisions[0].anchors.len(), 1);
        let by_y = list_changes(&wd, &an, &ListChangesArgs { author: Some("Y".into()), ..Default::default() }).unwrap();
        assert_eq!(by_y.revisions.len(), 1);
        let acc =
            list_changes(&wd, &an, &ListChangesArgs { status: Some("accepted".into()), ..Default::default() }).unwrap();
        assert!(acc.revisions.is_empty());
    }
}
