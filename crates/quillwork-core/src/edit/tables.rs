//! Table structure: rows, columns, cell text, merge / split, new tables (DESIGN §6.3).
//!
//! Track mode writes Word's table revisions: `w:trPr/w:ins|w:del` for rows, `w:tcPr/w:cellIns|
//! w:cellDel` plus `w:tblGridChange` for columns (beta: `confirm: true` required), and ordinary
//! run / paragraph-mark revisions for the cell content. Merging and splitting cells can't be
//! tracked in Word, so they are direct only.
//!
//! Row and cell anchors are positional (`t1.r2.c3`, 1-based); column numbers are grid columns.

use serde::Deserialize;

use super::review::{is_range_markup, remove_keeping_markup, remove_row};
use super::{
    EditOutcome, Guards, Mode, Tracker, Wq, clone_rpr, err, has_content, is_w, kids, make_run, mark_paragraph,
    order_index, track_delete_runs, w_attr, w_local,
};
use crate::anchors::{Anchors, Target};
use crate::error::Result;
use crate::wml::view::{Cell, Row, Table, VMerge};
use crate::wml::{Loc, PartIdx, TextMode, WordDoc, text};
use crate::xml::{Document, NodeId, ns};

// ---------------------------------------------------------------------- args

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InsertRowArgs {
    #[serde(flatten)]
    pub guards: Guards,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub cells: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeleteRowArgs {
    #[serde(flatten)]
    pub guards: Guards,
    pub row: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SetCellArgs {
    #[serde(flatten)]
    pub guards: Guards,
    pub cell: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InsertColumnArgs {
    #[serde(flatten)]
    pub guards: Guards,
    pub table: String,
    pub at: u32,
    #[serde(default)]
    pub cells: Option<Vec<String>>,
    #[serde(default)]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeleteColumnArgs {
    #[serde(flatten)]
    pub guards: Guards,
    pub table: String,
    pub column: u32,
    #[serde(default)]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MergeCellsArgs {
    #[serde(flatten)]
    pub guards: Guards,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SplitCellArgs {
    #[serde(flatten)]
    pub guards: Guards,
    pub cell: String,
    #[serde(default)]
    pub rows: Option<u32>,
    #[serde(default)]
    pub cols: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InsertTableArgs {
    #[serde(flatten)]
    pub guards: Guards,
    pub after: String,
    pub rows: u32,
    pub cols: u32,
    #[serde(default)]
    pub header_row: Option<bool>,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub data: Option<Vec<Vec<String>>>,
}

// ---------------------------------------------------------------------- resolution

fn resolve_table(doc: &WordDoc, anchors: &Anchors, h: &str) -> Result<Loc> {
    match anchors.resolve(doc, h) {
        Some(Target::Table { table }) => Ok(table),
        _ => Err(err("NOT_FOUND", format!("no table {h}"))),
    }
}

fn resolve_row(doc: &WordDoc, anchors: &Anchors, h: &str) -> Result<(Loc, NodeId)> {
    match anchors.resolve(doc, h) {
        Some(Target::Row { table, row }) => Ok((table, row.node)),
        _ => Err(err("NOT_FOUND", format!("no row {h}"))),
    }
}

fn resolve_cell(doc: &WordDoc, anchors: &Anchors, h: &str) -> Result<(Loc, NodeId, NodeId)> {
    match anchors.resolve(doc, h) {
        Some(Target::Cell { table, row, cell }) => Ok((table, row.node, cell.node)),
        _ => Err(err("NOT_FOUND", format!("no cell {h}"))),
    }
}

fn unsupported(reason: &str, msg: impl Into<String>) -> crate::error::Error {
    err("UNSUPPORTED", format!("{reason}: {}", msg.into()))
}

fn direct_only(args: &Guards, what: &str) -> Result<()> {
    if args.mode() == Mode::Track {
        return Err(unsupported("direct_only", format!("{what} can't be tracked in Word; use mode \"direct\"")));
    }
    Ok(())
}

// ---------------------------------------------------------------------- grid

fn int_val(d: &Document, parent: Option<NodeId>, local: &str, attr: &str) -> Option<i64> {
    let c = d.find_child(parent?, ns::W, local)?;
    w_attr(d, c, attr)?.trim().parse::<f64>().ok().map(|v| v as i64)
}

/// The `w:tblGrid` and its `w:gridCol`s.
fn grid(d: &Document, tbl: NodeId) -> (Option<NodeId>, Vec<NodeId>) {
    let g = d.find_child(tbl, ns::W, "tblGrid");
    let cols = g.map(|g| d.child_elements(g).filter(|&c| is_w(d, c, "gridCol")).collect()).unwrap_or_default();
    (g, cols)
}

fn col_width(d: &Document, col: NodeId) -> i64 {
    w_attr(d, col, "w").and_then(|v| v.trim().parse::<f64>().ok()).map(|v| v as i64).unwrap_or(0)
}

/// Cells of a row with their grid start and span (after `w:gridBefore`).
fn layout(d: &Document, tr: NodeId) -> Vec<(NodeId, usize, usize)> {
    let before = int_val(d, d.find_child(tr, ns::W, "trPr"), "gridBefore", "val").unwrap_or(0).max(0) as usize;
    let mut at = before;
    Row(tr)
        .cells(d)
        .into_iter()
        .map(|c| {
            let span = c.grid_span(d).max(1) as usize;
            let v = (c.0, at, span);
            at += span;
            v
        })
        .collect()
}

fn cell_at(d: &Document, tr: NodeId, start: usize) -> Option<(NodeId, usize, usize)> {
    layout(d, tr).into_iter().find(|&(_, s, _)| s == start)
}

// ---------------------------------------------------------------------- building blocks

/// Clean copy of a `w:pPr`: no section break, no tracked changes, minus `drop`.
fn clean_ppr(d: &mut Document, src: NodeId, drop: &[&str]) -> Result<NodeId> {
    let c = d.clone_subtree(src)?;
    for k in kids(d, c) {
        match w_local(d, k) {
            Some("sectPr" | "pPrChange") => d.remove(k)?,
            Some(l) if drop.contains(&l) => d.remove(k)?,
            Some("rPr") => {
                for x in kids(d, k) {
                    if matches!(w_local(d, x), Some("ins" | "del" | "moveFrom" | "moveTo" | "rPrChange")) {
                        d.remove(x)?;
                    }
                }
                if d.child_elements(k).next().is_none() {
                    d.remove(k)?;
                }
            }
            _ => {}
        }
    }
    if let Some(pre) = d.find_prefix(c, ns::W14).map(str::to_owned) {
        for a in ["paraId", "textId"] {
            d.remove_attr(c, &format!("{pre}:{a}"))?;
        }
    }
    Ok(c)
}

/// Clean copy of a `w:tcPr` (no revisions, no merge state unless kept).
fn clean_tcpr(d: &mut Document, src: NodeId, keep_span: bool) -> Result<NodeId> {
    let c = d.clone_subtree(src)?;
    for k in kids(d, c) {
        let drop = match w_local(d, k) {
            Some("cellIns" | "cellDel" | "cellMerge" | "tcPrChange" | "vMerge" | "hMerge") => true,
            Some("gridSpan") => !keep_span,
            _ => false,
        };
        if drop {
            d.remove(k)?;
        }
    }
    Ok(c)
}

/// Set `w:tcW` (dxa) in a `tcPr`, in schema position (after `cnfStyle`).
fn set_tcw(d: &mut Document, wq: &Wq, tcpr: NodeId, width: i64) -> Result<()> {
    let w = width.to_string();
    if let Some(tcw) = d.find_child(tcpr, ns::W, "tcW") {
        d.set_attr(tcw, &wq.q("w"), &w)?;
        d.set_attr(tcw, &wq.q("type"), "dxa")?;
        return Ok(());
    }
    let tcw = wq.el(d, "tcW", &[("w", &w), ("type", "dxa")])?;
    match d.find_child(tcpr, ns::W, "cnfStyle") {
        Some(c) => d.insert_after(c, tcw)?,
        None => d.prepend_child(tcpr, tcw)?,
    }
    Ok(())
}

/// Insert a `tcPr` child in schema order (CT_TcPr).
fn put_tcpr_child(d: &mut Document, tcpr: NodeId, child: NodeId) -> Result<()> {
    const ORDER: [&str; 17] = [
        "cnfStyle",
        "tcW",
        "gridSpan",
        "hMerge",
        "vMerge",
        "tcBorders",
        "shd",
        "noWrap",
        "tcMar",
        "textDirection",
        "tcFitText",
        "vAlign",
        "hideMark",
        "headers",
        "cellIns",
        "cellDel",
        "tcPrChange",
    ];
    let rank = |l: &str| ORDER.iter().position(|o| *o == l).unwrap_or(ORDER.len());
    let me = rank(w_local(d, child).unwrap_or(""));
    let next = d.child_elements(tcpr).find(|&c| rank(w_local(d, c).unwrap_or("")) > me);
    match next {
        Some(n) => d.insert_before(n, child),
        None => d.append_child(tcpr, child),
    }
}

fn tcpr_of(d: &mut Document, wq: &Wq, tc: NodeId) -> Result<NodeId> {
    if let Some(p) = d.find_child(tc, ns::W, "tcPr") {
        return Ok(p);
    }
    let p = wq.el(d, "tcPr", &[])?;
    d.prepend_child(tc, p)?;
    Ok(p)
}

/// The `w:trPr` of a row, created in place (after `tblPrEx`) when missing.
fn trpr_of(d: &mut Document, wq: &Wq, tr: NodeId) -> Result<NodeId> {
    if let Some(p) = d.find_child(tr, ns::W, "trPr") {
        return Ok(p);
    }
    let p = wq.el(d, "trPr", &[])?;
    match d.find_child(tr, ns::W, "tblPrEx") {
        Some(x) => d.insert_after(x, p)?,
        None => d.prepend_child(tr, p)?,
    }
    Ok(p)
}

/// Add a row revision marker (`ins` / `del`) to `trPr`, before `trPrChange`.
fn mark_row(d: &mut Document, wq: &Wq, tr: NodeId, kind: &str, id: u32, author: &str, date: &str) -> Result<()> {
    let trpr = trpr_of(d, wq, tr)?;
    let m = wq.rev(d, kind, id, author, date)?;
    match d.find_child(trpr, ns::W, "trPrChange") {
        Some(c) => d.insert_before(c, m),
        None => d.append_child(trpr, m),
    }
}

/// Revision context of a tracked edit.
struct Rv<'a> {
    author: &'a str,
    date: &'a str,
    ids: Vec<u32>,
}

impl Rv<'_> {
    fn id(&mut self, alloc: &mut dyn FnMut() -> u32) -> u32 {
        let id = alloc();
        self.ids.push(id);
        id
    }
}

/// Template of a cell's content: its first paragraph's `pPr` and its first text run's `rPr`.
fn cell_template(d: &Document, tc: NodeId) -> (Option<NodeId>, Option<NodeId>) {
    let p = d.descendants(tc).find(|&c| is_w(d, c, "p"));
    let ppr = p.and_then(|p| d.find_child(p, ns::W, "pPr"));
    let runs: Vec<NodeId> = d.descendants(tc).filter(|&c| is_w(d, c, "r")).collect();
    let run = runs
        .iter()
        .copied()
        .find(|&r| d.child_elements(r).any(|c| matches!(w_local(d, c), Some("t" | "delText"))))
        .or(runs.first().copied());
    (ppr, run.and_then(|r| d.find_child(r, ns::W, "rPr")))
}

/// Where a new node goes. New nodes are attached before they are filled: a detached element's
/// namespace is only bound once it is in the tree, and the `w:` lookups need it.
#[derive(Clone, Copy)]
enum Place {
    Append(NodeId),
    Before(NodeId),
    After(NodeId),
}

impl Place {
    fn put(self, d: &mut Document, n: NodeId) -> Result<()> {
        match self {
            Place::Append(p) => d.append_child(p, n),
            Place::Before(x) => d.insert_before(x, n),
            Place::After(x) => d.insert_after(x, n),
        }
    }
}

/// A new paragraph: cleaned copy of `ppr`, then `text` in one run with a copy of `rpr`. Tracked
/// (`rv`): the mark and the run are insertions.
#[allow(clippy::too_many_arguments)]
fn new_para(
    d: &mut Document,
    wq: &Wq,
    at: Place,
    ppr: Option<NodeId>,
    rpr: Option<NodeId>,
    text: &str,
    rv: Option<&mut Rv>,
    alloc: &mut dyn FnMut() -> u32,
) -> Result<NodeId> {
    let p = wq.el(d, "p", &[])?;
    at.put(d, p)?;
    if let Some(src) = ppr {
        let c = clean_ppr(d, src, &[])?;
        d.append_child(p, c)?;
    }
    let mut rv = rv;
    if let Some(rv) = rv.as_deref_mut() {
        let id = rv.id(alloc);
        mark_paragraph(d, wq, p, "ins", id, rv.author, rv.date)?;
    }
    if !text.is_empty() {
        let rpr = match rpr {
            Some(r) => Some(clone_rpr(d, r)?),
            None => None,
        };
        let run = make_run(d, wq, rpr, text, false)?;
        match rv {
            Some(rv) => {
                let id = rv.id(alloc);
                let ins = wq.rev(d, "ins", id, rv.author, rv.date)?;
                d.append_child(ins, run)?;
                d.append_child(p, ins)?;
            }
            None => d.append_child(p, run)?,
        }
    }
    Ok(p)
}

/// One paragraph per line of `text`.
#[allow(clippy::too_many_arguments)]
fn new_paras(
    d: &mut Document,
    wq: &Wq,
    at: Place,
    ppr: Option<NodeId>,
    rpr: Option<NodeId>,
    text: &str,
    mut rv: Option<&mut Rv>,
    alloc: &mut dyn FnMut() -> u32,
) -> Result<Vec<NodeId>> {
    let mut out: Vec<NodeId> = Vec::new();
    for line in text.split('\n') {
        let at = out.last().map_or(at, |&p| Place::After(p));
        out.push(new_para(d, wq, at, ppr, rpr, line.trim_end_matches('\r'), rv.as_deref_mut(), alloc)?);
    }
    Ok(out)
}

/// A new cell modelled on `src` (its `tcPr`, first paragraph and run formatting).
#[allow(clippy::too_many_arguments)]
fn new_cell(
    d: &mut Document,
    wq: &Wq,
    at: Place,
    src: NodeId,
    keep_span: bool,
    width: Option<i64>,
    text: &str,
    rv: Option<&mut Rv>,
    alloc: &mut dyn FnMut() -> u32,
) -> Result<NodeId> {
    let tc = wq.el(d, "tc", &[])?;
    at.put(d, tc)?;
    let tcpr = match d.find_child(src, ns::W, "tcPr") {
        Some(p) => clean_tcpr(d, p, keep_span)?,
        None => wq.el(d, "tcPr", &[])?,
    };
    d.append_child(tc, tcpr)?;
    if let Some(w) = width {
        set_tcw(d, wq, tcpr, w)?;
    }
    let (ppr, rpr) = cell_template(d, src);
    new_paras(d, wq, Place::Append(tc), ppr, rpr, text, rv, alloc)?;
    if d.child_elements(tcpr).next().is_none() {
        d.remove(tcpr)?;
    }
    Ok(tc)
}

/// Move the range markup below `n` (bookmarks, comment ranges, …) to just before `before`.
fn salvage_markup(d: &mut Document, n: NodeId, before: NodeId) -> Result<()> {
    let keep: Vec<NodeId> = d.descendants(n).filter(|&c| c != n && is_range_markup(d, c)).collect();
    for c in keep {
        d.insert_before(before, c)?;
    }
    Ok(())
}

/// Stamp `w14:paraId` on new paragraphs when the part already uses them.
fn stamp_para_ids(doc: &mut WordDoc, part: PartIdx, paras: &[NodeId]) -> Result<()> {
    let d = doc.part(part);
    let uses = d.descendants(d.root()).any(|c| is_w(d, c, "p") && d.attr_ns(c, ns::W14, "paraId").is_some());
    if uses {
        for &p in paras {
            if attached(doc.part(part), p) {
                doc.ensure_para_id(Loc::new(part, p))?;
            }
        }
    }
    Ok(())
}

fn attached(d: &Document, n: NodeId) -> bool {
    super::attached(d, n)
}

fn paras_in(d: &Document, n: NodeId) -> Vec<NodeId> {
    d.descendants(n).filter(|&c| is_w(d, c, "p")).collect()
}

fn mark_deleted(d: &Document, p: NodeId) -> bool {
    d.find_child(p, ns::W, "pPr")
        .and_then(|ppr| d.find_child(ppr, ns::W, "rPr"))
        .is_some_and(|r| d.find_child(r, ns::W, "del").is_some())
}

/// Track-delete everything in `n`: runs into `w:del`, every paragraph mark deleted.
fn track_delete_all(
    d: &mut Document,
    wq: &Wq,
    n: NodeId,
    rv: &mut Rv,
    alloc: &mut dyn FnMut() -> u32,
    marks: bool,
) -> Result<()> {
    let ids = track_delete_runs(d, wq, n, alloc, rv.author, rv.date)?;
    rv.ids.extend(ids);
    if marks {
        for p in paras_in(d, n) {
            if !mark_deleted(d, p) {
                let id = rv.id(alloc);
                mark_paragraph(d, wq, p, "del", id, rv.author, rv.date)?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------- rows

/// `insert_row`: a new row after / before the anchor row, modelled on it.
pub fn insert_row(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    alloc_rev: &mut dyn FnMut() -> u32,
    author: &str,
    date: &str,
    args: &InsertRowArgs,
) -> Result<EditOutcome> {
    let (h, after) = match (&args.after, &args.before) {
        (Some(h), None) => (h, true),
        (None, Some(h)) => (h, false),
        _ => return Err(err("INVALID_OP", "insert_row takes exactly one of after / before")),
    };
    let (tbl, tr) = resolve_row(doc, anchors, h)?;
    let part = tbl.part;
    let track = args.guards.mode() == Mode::Track;
    let tracker = Tracker::new(anchors);
    let mut rv = Rv { author, date, ids: Vec::new() };
    let new_paras_list;
    {
        let d = doc.part_mut(part);
        let wq = Wq::of(d);
        let cells = layout(d, tr);
        let texts = args.cells.clone().unwrap_or_default();
        if texts.len() > cells.len() {
            return Err(err("INVALID_OP", format!("{} cell texts for a row of {} cells", texts.len(), cells.len())));
        }
        // The row after the insertion point decides whether a vertical merge runs through it.
        let rows = Table(tbl.node).rows(d);
        let at = rows.iter().position(|r| r.0 == tr).unwrap_or(0);
        let next_row = if after { rows.get(at + 1).map(|r| r.0) } else { Some(tr) };
        let continues: Vec<bool> = cells
            .iter()
            .map(|&(tc, start, _)| {
                next_row
                    .and_then(|nr| cell_at(d, nr, start))
                    .is_some_and(|(c, _, _)| Cell(c).v_merge(d) == VMerge::Continue)
                    && (!after || Cell(tc).v_merge(d) != VMerge::None)
            })
            .collect();
        for (i, &continued) in continues.iter().enumerate() {
            if continued && texts.get(i).is_some_and(|s| !s.is_empty()) {
                return Err(unsupported(
                    "merged_cells",
                    format!("cell {} continues a vertical merge; supply empty text or insert outside the merge", i + 1),
                ));
            }
        }

        let new_tr = wq.el(d, "tr", &[])?;
        if after {
            d.insert_after(tr, new_tr)?
        } else {
            d.insert_before(tr, new_tr)?
        }
        if let Some(x) = d.find_child(tr, ns::W, "tblPrEx") {
            let c = d.clone_subtree(x)?;
            for k in kids(d, c) {
                if is_w(d, k, "tblPrExChange") {
                    d.remove(k)?;
                }
            }
            d.append_child(new_tr, c)?;
        }
        if let Some(x) = d.find_child(tr, ns::W, "trPr") {
            let c = d.clone_subtree(x)?;
            for k in kids(d, c) {
                if matches!(w_local(d, k), Some("ins" | "del" | "trPrChange" | "tblHeader" | "cnfStyle")) {
                    d.remove(k)?;
                }
            }
            if d.child_elements(c).next().is_some() {
                d.append_child(new_tr, c)?;
            }
        }
        if track {
            let id = rv.id(alloc_rev);
            mark_row(d, &wq, new_tr, "ins", id, author, date)?;
        }
        for (i, &(tc, _, _)) in cells.iter().enumerate() {
            let text = texts.get(i).map(String::as_str).unwrap_or("");
            let c = new_cell(d, &wq, Place::Append(new_tr), tc, true, None, text, track.then_some(&mut rv), alloc_rev)?;
            if continues[i] {
                let tcpr = tcpr_of(d, &wq, c)?;
                let vm = wq.el(d, "vMerge", &[])?;
                put_tcpr_child(d, tcpr, vm)?;
            }
        }
        new_paras_list = paras_in(d, new_tr);
    }
    stamp_para_ids(doc, part, &new_paras_list)?;
    let mut out = tracker.finish(doc, anchors, rv.ids);
    out.changed.clear();
    Ok(out)
}

/// `delete_row`.
pub fn delete_row(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    alloc_rev: &mut dyn FnMut() -> u32,
    author: &str,
    date: &str,
    args: &DeleteRowArgs,
) -> Result<EditOutcome> {
    let (tbl, tr) = resolve_row(doc, anchors, &args.row)?;
    let part = tbl.part;
    let mut tracker = Tracker::new(anchors);
    tracker.touch_all_paragraphs(doc, part, tr);
    let mut rv = Rv { author, date, ids: Vec::new() };
    {
        let d = doc.part_mut(part);
        let wq = Wq::of(d);
        let trpr = d.find_child(tr, ns::W, "trPr");
        let has = |l: &str| trpr.is_some_and(|p| d.find_child(p, ns::W, l).is_some());
        let (inserted, deleted) = (has("ins"), has("del"));
        if args.guards.mode() == Mode::Track && !inserted {
            if deleted {
                return Ok(EditOutcome::default());
            }
            let id = rv.id(alloc_rev);
            mark_row(d, &wq, tr, "del", id, author, date)?;
            track_delete_all(d, &wq, tr, &mut rv, alloc_rev, true)?;
        } else {
            // Direct (or deleting a tracked insertion): the row goes, its range markup stays.
            let last = Table(tbl.node).rows(d).len() == 1;
            let before = if last { tbl.node } else { tr };
            salvage_markup(d, tr, before)?;
            remove_row(d, &wq, tr)?;
        }
    }
    Ok(tracker.finish(doc, anchors, rv.ids))
}

// ---------------------------------------------------------------------- cell text

/// `set_cell`: replace the cell's text (one paragraph per line), keeping the first paragraph's
/// properties and the first run's formatting.
pub fn set_cell(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    alloc_rev: &mut dyn FnMut() -> u32,
    author: &str,
    date: &str,
    args: &SetCellArgs,
) -> Result<EditOutcome> {
    let (tbl, _, tc) = resolve_cell(doc, anchors, &args.cell)?;
    let part = tbl.part;
    {
        let d = doc.part(part);
        if d.descendants(tc).any(|c| c != tc && is_w(d, c, "tbl")) {
            return Err(unsupported("nested_table", "the cell holds a table; edit its paragraphs instead"));
        }
        if text::text(d, tc, TextMode::AcceptAll) == args.text {
            return Ok(EditOutcome::default());
        }
    }
    let mut tracker = Tracker::new(anchors);
    tracker.touch_all_paragraphs(doc, part, tc);
    let mut rv = Rv { author, date, ids: Vec::new() };
    let created;
    {
        let d = doc.part_mut(part);
        let wq = Wq::of(d);
        let paras: Vec<NodeId> = d.child_elements(tc).filter(|&c| is_w(d, c, "p")).collect();
        let (ppr, rpr) = cell_template(d, tc);
        let ppr = ppr.map(|p| d.clone_subtree(p)).transpose()?;
        let rpr = rpr.map(|r| d.clone_subtree(r)).transpose()?;
        let lines: Vec<&str> = args.text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
        let mut made = Vec::new();
        if args.guards.mode() == Mode::Track {
            // Old content deleted; all marks but the last deleted; new lines as inserted
            // paragraphs before the last one, whose run takes the final line.
            track_delete_all(d, &wq, tc, &mut rv, alloc_rev, false)?;
            let last = *paras.last().ok_or_else(|| err("INVALID_OP", "cell without a paragraph"))?;
            for &p in &paras[..paras.len() - 1] {
                if !mark_deleted(d, p) {
                    let id = rv.id(alloc_rev);
                    mark_paragraph(d, &wq, p, "del", id, author, date)?;
                }
            }
            for line in &lines[..lines.len() - 1] {
                let p = new_para(d, &wq, Place::Before(last), ppr, rpr, line, Some(&mut rv), alloc_rev)?;
                made.push(p);
            }
            let fin = lines[lines.len() - 1];
            if !fin.is_empty() {
                let r = match rpr {
                    Some(r) => Some(clone_rpr(d, r)?),
                    None => None,
                };
                let run = make_run(d, &wq, r, fin, false)?;
                let id = rv.id(alloc_rev);
                let ins = wq.rev(d, "ins", id, author, date)?;
                d.append_child(ins, run)?;
                d.append_child(last, ins)?;
            }
        } else {
            let first = *paras.first().ok_or_else(|| err("INVALID_OP", "cell without a paragraph"))?;
            for &p in &paras[1..] {
                salvage_markup(d, p, p)?;
                d.remove(p)?;
            }
            for c in kids(d, first) {
                if !is_w(d, c, "pPr") {
                    remove_keeping_markup(d, c)?;
                }
            }
            if !lines[0].is_empty() {
                let r = match rpr {
                    Some(r) => Some(clone_rpr(d, r)?),
                    None => None,
                };
                let run = make_run(d, &wq, r, lines[0], false)?;
                d.append_child(first, run)?;
            }
            let mut prev = first;
            for line in &lines[1..] {
                let p = new_para(d, &wq, Place::After(prev), ppr, rpr, line, None, alloc_rev)?;
                prev = p;
                made.push(p);
            }
        }
        for n in [ppr, rpr].into_iter().flatten() {
            d.remove(n)?;
        }
        created = made;
    }
    stamp_para_ids(doc, part, &created)?;
    Ok(tracker.finish(doc, anchors, rv.ids))
}

// ---------------------------------------------------------------------- columns

fn confirm_beta(guards: &Guards, confirm: Option<bool>, table: &str) -> Result<()> {
    if guards.mode() == Mode::Track && confirm != Some(true) {
        return Err(err(
            "CONFIRM_REQUIRED",
            format!("beta: tracked column edits show unevenly in Word; resend with confirm: true ({table})"),
        ));
    }
    Ok(())
}

/// Record the current grid in a `w:tblGridChange` (tracked grid edit).
fn grid_change(d: &mut Document, wq: &Wq, grid: NodeId, id: u32) -> Result<()> {
    if d.find_child(grid, ns::W, "tblGridChange").is_some() {
        return Ok(()); // the original grid is already on record
    }
    let ch = wq.el(d, "tblGridChange", &[("id", &id.to_string())])?;
    let old = wq.el(d, "tblGrid", &[])?;
    for c in kids(d, grid) {
        if is_w(d, c, "gridCol") {
            let x = d.clone_subtree(c)?;
            d.append_child(old, x)?;
        }
    }
    d.append_child(ch, old)?;
    d.append_child(grid, ch)?;
    Ok(())
}

/// `insert_column` (beta): a new grid column at `at` (1-based), one cell per row.
pub fn insert_column(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    alloc_rev: &mut dyn FnMut() -> u32,
    author: &str,
    date: &str,
    args: &InsertColumnArgs,
) -> Result<EditOutcome> {
    confirm_beta(&args.guards, args.confirm, &args.table)?;
    let tbl = resolve_table(doc, anchors, &args.table)?;
    let part = tbl.part;
    let track = args.guards.mode() == Mode::Track;
    let mut tracker = Tracker::new(anchors);
    let mut rv = Rv { author, date, ids: Vec::new() };
    let made;
    {
        let d = doc.part_mut(part);
        let wq = Wq::of(d);
        let (g, cols) = grid(d, tbl.node);
        let g = g.ok_or_else(|| unsupported("no_grid", "table without w:tblGrid"))?;
        let n = cols.len();
        if args.at == 0 || args.at as usize > n + 1 {
            return Err(err("INVALID_OP", format!("column {} out of range 1..={}", args.at, n + 1)));
        }
        let b = args.at as usize - 1;
        let rows = Table(tbl.node).rows(d);
        let texts = args.cells.clone().unwrap_or_default();
        if texts.len() > rows.len() {
            return Err(err("INVALID_OP", format!("{} cell texts for {} rows", texts.len(), rows.len())));
        }
        // Plan first: (row, neighbour cell, insert before it?).
        let mut plan = Vec::new();
        for r in &rows {
            let lay = layout(d, r.0);
            let place = if let Some(&(tc, _, _)) = lay.iter().find(|&&(_, s, _)| s == b) {
                (tc, true)
            } else if let Some(&(tc, s, sp)) = lay.last().filter(|&&(_, s, sp)| s + sp == b) {
                let _ = (s, sp);
                (tc, false)
            } else {
                return Err(unsupported(
                    "merged_cells",
                    format!("column boundary {} falls inside a merged cell", args.at),
                ));
            };
            plan.push((r.0, place.0, place.1));
        }
        let widths: Vec<i64> = cols.iter().map(|&c| col_width(d, c)).collect();
        let width = if n == 0 { 1000 } else { widths.iter().sum::<i64>() / n as i64 };
        if track {
            let id = rv.id(alloc_rev);
            grid_change(d, &wq, g, id)?;
        }
        let gc = wq.el(d, "gridCol", &[("w", &width.to_string())])?;
        match cols.get(b) {
            Some(&c) => d.insert_before(c, gc)?,
            None => match cols.last() {
                Some(&l) => d.insert_after(l, gc)?,
                None => d.prepend_child(g, gc)?,
            },
        }
        let mut new = Vec::new();
        for (i, (tr, nb, before)) in plan.into_iter().enumerate() {
            let text = texts.get(i).map(String::as_str).unwrap_or("");
            let at = if before { Place::Before(nb) } else { Place::After(nb) };
            let tc = new_cell(d, &wq, at, nb, false, Some(width), text, track.then_some(&mut rv), alloc_rev)?;
            if track {
                let id = rv.id(alloc_rev);
                let ci = wq.rev(d, "cellIns", id, author, date)?;
                let tcpr = tcpr_of(d, &wq, tc)?;
                put_tcpr_child(d, tcpr, ci)?;
            }
            let _ = tr;
            new.extend(paras_in(d, tc));
        }
        made = new;
    }
    tracker.touch(doc, part, tbl.node);
    stamp_para_ids(doc, part, &made)?;
    let mut out = tracker.finish(doc, anchors, rv.ids);
    out.changed.retain(|h| h == &args.table);
    if out.changed.is_empty() {
        out.changed.push(args.table.clone());
    }
    Ok(out)
}

/// `delete_column` (beta): the cells of grid column `column` (1-based). Tracked, each cell gets
/// `w:cellDel` and its content is deleted; the grid shrinks when the deletion is accepted.
pub fn delete_column(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    alloc_rev: &mut dyn FnMut() -> u32,
    author: &str,
    date: &str,
    args: &DeleteColumnArgs,
) -> Result<EditOutcome> {
    confirm_beta(&args.guards, args.confirm, &args.table)?;
    let tbl = resolve_table(doc, anchors, &args.table)?;
    let part = tbl.part;
    let mut tracker = Tracker::new(anchors);
    let mut rv = Rv { author, date, ids: Vec::new() };
    {
        let d = doc.part_mut(part);
        let wq = Wq::of(d);
        let (_, cols) = grid(d, tbl.node);
        let c = args.column as usize;
        if c == 0 || c > cols.len() {
            return Err(err("INVALID_OP", format!("column {c} out of range 1..={}", cols.len())));
        }
        if cols.len() == 1 {
            return Err(err("INVALID_OP", "a table's last column can't be deleted; delete the table"));
        }
        let mut cells = Vec::new();
        for r in Table(tbl.node).rows(d) {
            let lay = layout(d, r.0);
            match lay.iter().find(|&&(_, s, sp)| s < c && c - 1 < s + sp) {
                Some(&(tc, _, 1)) => cells.push(tc),
                Some(_) => return Err(unsupported("merged_cells", format!("column {c} crosses a merged cell"))),
                None => {} // short row (gridBefore / gridAfter): nothing in this column
            }
        }
        for &tc in &cells {
            for p in paras_in(d, tc) {
                tracker.touched.push(Loc::new(part, p));
            }
        }
        if args.guards.mode() == Mode::Track {
            for tc in cells {
                let tcpr = tcpr_of(d, &wq, tc)?;
                if d.find_child(tcpr, ns::W, "cellDel").is_some() {
                    continue;
                }
                let id = rv.id(alloc_rev);
                let cd = wq.rev(d, "cellDel", id, author, date)?;
                put_tcpr_child(d, tcpr, cd)?;
                track_delete_all(d, &wq, tc, &mut rv, alloc_rev, true)?;
            }
        } else {
            for tc in cells {
                salvage_markup(d, tc, tc)?;
                d.remove(tc)?;
            }
            let (_, cols) = grid(d, tbl.node);
            d.remove(cols[c - 1])?;
        }
    }
    let mut out = tracker.finish(doc, anchors, rv.ids);
    out.changed.insert(0, args.table.clone());
    Ok(out)
}

/// After a cell was removed (accepting `cellDel`, rejecting `cellIns`): drop the grid column
/// it occupied once no row reaches the grid's end any more. A grid under its own revision
/// (`tblGridChange`) is left to that revision.
pub(crate) fn shrink_grid_after_cell_removal(d: &mut Document, tbl: NodeId, start: usize) -> Result<()> {
    let (g, cols) = grid(d, tbl);
    let Some(g) = g else { return Ok(()) };
    if start >= cols.len() || d.find_child(g, ns::W, "tblGridChange").is_some() {
        return Ok(());
    }
    let widest = Table(tbl)
        .rows(d)
        .iter()
        .map(|r| {
            let after = int_val(d, d.find_child(r.0, ns::W, "trPr"), "gridAfter", "val").unwrap_or(0).max(0) as usize;
            layout(d, r.0).last().map_or(0, |&(_, s, sp)| s + sp) + after
        })
        .max()
        .unwrap_or(0);
    if widest < cols.len() {
        d.remove(cols[start])?;
    }
    Ok(())
}

/// Grid start of a cell in its row.
pub(crate) fn cell_grid_start(d: &Document, tc: NodeId) -> Option<(NodeId, usize)> {
    let tr = d.ancestors(tc).find(|&a| is_w(d, a, "tr"))?;
    let tbl = d.ancestors(tr).find(|&a| is_w(d, a, "tbl"))?;
    let s = layout(d, tr).into_iter().find(|&(c, _, _)| c == tc)?.1;
    Some((tbl, s))
}

// ---------------------------------------------------------------------- merge / split

/// Row index and grid range of a cell.
fn cell_pos(d: &Document, tbl: NodeId, tr: NodeId, tc: NodeId) -> Option<(usize, usize, usize)> {
    let r = Table(tbl).rows(d).iter().position(|x| x.0 == tr)?;
    let (_, s, sp) = layout(d, tr).into_iter().find(|&(c, _, _)| c == tc)?;
    Some((r, s, s + sp))
}

/// Move the content of `from` (all but empty paragraphs) to the end of `into`; leave `from`
/// with one empty paragraph (its first paragraph's properties kept).
fn move_content(d: &mut Document, wq: &Wq, from: NodeId, into: NodeId) -> Result<()> {
    let blocks: Vec<NodeId> = d.child_elements(from).filter(|&c| !is_w(d, c, "tcPr")).collect();
    let ppr = blocks.iter().find(|&&b| is_w(d, b, "p")).and_then(|&p| d.find_child(p, ns::W, "pPr"));
    let keep_ppr = ppr.map(|p| clean_ppr(d, p, &[])).transpose()?;
    let any = blocks.iter().any(|&b| !is_w(d, b, "p") || has_content(d, b));
    if any {
        // Drop a trailing empty paragraph of the target first.
        let last = d.child_elements(into).filter(|&c| is_w(d, c, "p")).last();
        if let Some(l) = last
            .filter(|&l| !has_content(d, l) && d.child_elements(into).filter(|&c| is_w(d, c, "p")).count() > 1 || false)
        {
            let _ = l;
        }
        let tlast = d.child_elements(into).last();
        let lone_empty = tlast.is_some_and(|l| is_w(d, l, "p") && !has_content(d, l))
            && d.child_elements(into).filter(|&c| !is_w(d, c, "tcPr")).count() == 1;
        for b in &blocks {
            if is_w(d, *b, "p") && !has_content(d, *b) && !d.descendants(*b).any(|c| is_range_markup(d, c)) {
                continue;
            }
            d.append_child(into, *b)?;
        }
        if lone_empty
            && let Some(l) = tlast
            && !d.descendants(l).any(|c| is_range_markup(d, c))
        {
            d.remove(l)?;
        }
    }
    for b in kids(d, from) {
        if !is_w(d, b, "tcPr") {
            d.remove(b)?;
        }
    }
    let p = wq.el(d, "p", &[])?;
    if let Some(pp) = keep_ppr {
        d.append_child(p, pp)?;
    }
    d.append_child(from, p)?;
    Ok(())
}

/// `merge_cells` (direct only): the rectangle `from`..`to` becomes one cell — `gridSpan` across,
/// `vMerge` down. Content moves to the top-left cell.
pub fn merge_cells(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    _alloc_rev: &mut dyn FnMut() -> u32,
    _author: &str,
    _date: &str,
    args: &MergeCellsArgs,
) -> Result<EditOutcome> {
    direct_only(&args.guards, "merging cells")?;
    let (t1, r1, c1) = resolve_cell(doc, anchors, &args.from)?;
    let (t2, r2, c2) = resolve_cell(doc, anchors, &args.to)?;
    if t1 != t2 {
        return Err(err("INVALID_OP", "from and to are in different tables"));
    }
    let part = t1.part;
    let tbl = t1.node;
    let mut tracker = Tracker::new(anchors);
    {
        let d = doc.part_mut(part);
        let wq = Wq::of(d);
        let (ra, ga, _) = cell_pos(d, tbl, r1, c1).ok_or_else(|| err("NOT_FOUND", "from cell"))?;
        let (rb, _, gb) = cell_pos(d, tbl, r2, c2).ok_or_else(|| err("NOT_FOUND", "to cell"))?;
        if rb < ra || gb <= ga {
            return Err(err("INVALID_OP", "to must be below / right of from"));
        }
        if (ra, ga) == (rb, gb - 1) && c1 == c2 {
            return Ok(EditOutcome::default());
        }
        let rows = Table(tbl).rows(d);
        let (_, cols) = grid(d, tbl);
        // Plan: per row, the cells exactly covering [ga, gb).
        let mut plan: Vec<Vec<NodeId>> = Vec::new();
        for r in &rows[ra..=rb] {
            let lay = layout(d, r.0);
            let inside: Vec<_> = lay.iter().filter(|&&(_, s, sp)| s < gb && s + sp > ga).collect();
            let aligned = inside.first().is_some_and(|f| f.1 == ga) && inside.last().is_some_and(|l| l.1 + l.2 == gb);
            if !aligned {
                return Err(unsupported("unaligned_cells", "the range cuts through a merged cell"));
            }
            if inside.iter().any(|&&(c, _, _)| Cell(c).v_merge(d) != VMerge::None) {
                return Err(unsupported("merged_cells", "the range holds vertically merged cells"));
            }
            plan.push(inside.iter().map(|x| x.0).collect());
        }
        let width: i64 = cols.get(ga..gb).map(|c| c.iter().map(|&x| col_width(d, x)).sum()).unwrap_or(0);
        let top_left = plan[0][0];
        for (ri, cells) in plan.iter().enumerate() {
            for &c in cells {
                tracker.touched.extend(paras_in(d, c).into_iter().map(|p| Loc::new(part, p)));
            }
            let first = cells[0];
            for &c in &cells[1..] {
                move_content(d, &wq, c, first)?;
                salvage_markup(d, c, c)?;
                d.remove(c)?;
            }
            let tcpr = tcpr_of(d, &wq, first)?;
            if let Some(gs) = d.find_child(tcpr, ns::W, "gridSpan") {
                d.remove(gs)?;
            }
            if gb - ga > 1 {
                let gs = wq.el(d, "gridSpan", &[("val", &(gb - ga).to_string())])?;
                put_tcpr_child(d, tcpr, gs)?;
            }
            if width > 0 {
                set_tcw(d, &wq, tcpr, width)?;
            }
            if rb > ra {
                let vm = if ri == 0 { wq.el(d, "vMerge", &[("val", "restart")])? } else { wq.el(d, "vMerge", &[])? };
                put_tcpr_child(d, tcpr, vm)?;
                if ri > 0 {
                    move_content(d, &wq, first, top_left)?;
                }
            }
        }
    }
    Ok(tracker.finish(doc, anchors, Vec::new()))
}

/// `split_cell` (direct only): `cols` splits across (a spanning cell shares its span; a
/// single-column cell gets new grid columns, the other rows' cells widen to match); `rows`
/// splits down (a vertically merged cell is unmerged; otherwise new rows are added and the
/// row's other cells merge down over them).
pub fn split_cell(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    alloc_rev: &mut dyn FnMut() -> u32,
    _author: &str,
    _date: &str,
    args: &SplitCellArgs,
) -> Result<EditOutcome> {
    direct_only(&args.guards, "splitting cells")?;
    let (tl, tr, tc) = resolve_cell(doc, anchors, &args.cell)?;
    let (part, tbl) = (tl.part, tl.node);
    let cols = args.cols.unwrap_or(1).max(1) as usize;
    let rows_n = args.rows.unwrap_or(1).max(1) as usize;
    if args.cols.is_none() && args.rows.is_none() {
        return Err(err("INVALID_OP", "split_cell needs rows or cols"));
    }
    let mut tracker = Tracker::new(anchors);
    tracker.touch_all_paragraphs(doc, part, tc);
    let mut made: Vec<NodeId> = Vec::new();
    {
        let d = doc.part_mut(part);
        let wq = Wq::of(d);
        let mut cells_now = vec![tc];
        if cols > 1 {
            let (_, s, e) = cell_pos(d, tbl, tr, tc).ok_or_else(|| err("NOT_FOUND", "cell"))?;
            let span = e - s;
            let (g, gcols) = grid(d, tbl);
            let width: i64 = gcols.get(s..e).map(|c| c.iter().map(|&x| col_width(d, x)).sum()).unwrap_or(0);
            if span % cols == 0 {
                // Share the span.
                let each = span / cols;
                set_span(d, &wq, tc, each)?;
                for k in 1..cols {
                    let prev = cells_now[k - 1];
                    let nc = new_cell(d, &wq, Place::After(prev), tc, false, None, "", None, alloc_rev)?;
                    set_span(d, &wq, nc, each)?;
                    cells_now.push(nc);
                }
                let w: Vec<i64> = (0..cols)
                    .map(|k| gcols[s + k * each..s + (k + 1) * each].iter().map(|&x| col_width(d, x)).sum())
                    .collect();
                for (k, &c) in cells_now.iter().enumerate() {
                    let tcpr = tcpr_of(d, &wq, c)?;
                    set_tcw(d, &wq, tcpr, w[k])?;
                }
            } else if span == 1 {
                // New grid columns: split this column's width; other rows' cells widen.
                let g = g.ok_or_else(|| unsupported("no_grid", "table without w:tblGrid"))?;
                let _ = g;
                let each = (width / cols as i64).max(1);
                let col = gcols[s];
                d.set_attr(col, &wq.q("w"), &each.to_string())?;
                let mut prev = col;
                for _ in 1..cols {
                    let gc = wq.el(d, "gridCol", &[("w", &each.to_string())])?;
                    d.insert_after(prev, gc)?;
                    prev = gc;
                }
                for r in Table(tbl).rows(d) {
                    if r.0 == tr {
                        continue;
                    }
                    if let Some(&(c, _, sp)) = layout(d, r.0).iter().find(|&&(_, cs, sp)| cs <= s && s < cs + sp) {
                        set_span(d, &wq, c, sp + cols - 1)?;
                    }
                }
                let tcpr = tcpr_of(d, &wq, tc)?;
                set_tcw(d, &wq, tcpr, each)?;
                for k in 1..cols {
                    let prev = cells_now[k - 1];
                    let nc = new_cell(d, &wq, Place::After(prev), tc, false, Some(each), "", None, alloc_rev)?;
                    cells_now.push(nc);
                }
            } else {
                return Err(unsupported(
                    "uneven_split",
                    format!("a cell spanning {span} columns can't split into {cols}"),
                ));
            }
        }
        if rows_n > 1 {
            let rows = Table(tbl).rows(d);
            let ri = rows.iter().position(|r| r.0 == tr).ok_or_else(|| err("NOT_FOUND", "row"))?;
            let s = cell_pos(d, tbl, tr, tc).map(|p| p.1).unwrap_or(0);
            // A vertically merged cell of exactly `rows_n` rows: unmerge.
            let mut chain = vec![tc];
            if Cell(tc).v_merge(d) == VMerge::Restart {
                for r in &rows[ri + 1..] {
                    match cell_at(d, r.0, s) {
                        Some((c, _, _)) if Cell(c).v_merge(d) == VMerge::Continue => chain.push(c),
                        _ => break,
                    }
                }
            }
            if chain.len() > 1 {
                if chain.len() != rows_n || cols > 1 {
                    return Err(unsupported("uneven_split", format!("the merged cell spans {} rows", chain.len())));
                }
                for c in chain {
                    if let Some(vm) = d.find_child(d.find_child(c, ns::W, "tcPr").unwrap_or(c), ns::W, "vMerge") {
                        d.remove(vm)?;
                    }
                }
            } else {
                if Cell(tc).v_merge(d) != VMerge::None {
                    return Err(unsupported("merged_cells", "the cell continues a vertical merge"));
                }
                // New rows below; the row's other cells merge down over them.
                let lay = layout(d, tr);
                for &(c, _, _) in &lay {
                    if cells_now.contains(&c) {
                        continue;
                    }
                    if Cell(c).v_merge(d) == VMerge::None {
                        let tcpr = tcpr_of(d, &wq, c)?;
                        let vm = wq.el(d, "vMerge", &[("val", "restart")])?;
                        put_tcpr_child(d, tcpr, vm)?;
                    }
                }
                let mut prev = tr;
                for _ in 1..rows_n {
                    let nr = wq.el(d, "tr", &[])?;
                    d.insert_after(prev, nr)?;
                    if let Some(x) = d.find_child(tr, ns::W, "trPr") {
                        let c = d.clone_subtree(x)?;
                        for k in kids(d, c) {
                            if matches!(w_local(d, k), Some("ins" | "del" | "trPrChange" | "tblHeader")) {
                                d.remove(k)?;
                            }
                        }
                        d.append_child(nr, c)?;
                    }
                    for &(c, _, _) in &layout(d, tr) {
                        let nc = new_cell(d, &wq, Place::Append(nr), c, true, None, "", None, alloc_rev)?;
                        if !cells_now.contains(&c) {
                            let tcpr = tcpr_of(d, &wq, nc)?;
                            let vm = wq.el(d, "vMerge", &[])?;
                            put_tcpr_child(d, tcpr, vm)?;
                        }
                    }
                    made.extend(paras_in(d, nr));
                    prev = nr;
                }
            }
        }
        for &c in &cells_now[1..] {
            made.extend(paras_in(d, c));
        }
    }
    stamp_para_ids(doc, part, &made)?;
    let mut out = tracker.finish(doc, anchors, Vec::new());
    out.changed.retain(|h| !h.is_empty());
    Ok(out)
}

fn set_span(d: &mut Document, wq: &Wq, tc: NodeId, span: usize) -> Result<()> {
    let tcpr = tcpr_of(d, wq, tc)?;
    if let Some(gs) = d.find_child(tcpr, ns::W, "gridSpan") {
        d.remove(gs)?;
    }
    if span > 1 {
        let gs = wq.el(d, "gridSpan", &[("val", &span.to_string())])?;
        put_tcpr_child(d, tcpr, gs)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------- new table

/// Text width (twips) of the section holding `n`: page width minus the side margins.
fn text_width(d: &Document, n: NodeId) -> i64 {
    let rank = order_index(d);
    let me = rank.get(&n).copied().unwrap_or(0);
    let sect = d
        .descendants(d.root())
        .filter(|&c| is_w(d, c, "sectPr") && d.parent(c).is_some_and(|p| is_w(d, p, "pPr") || is_w(d, p, "body")))
        .find(|c| rank.get(c).copied().unwrap_or(0) > me);
    let Some(s) = sect else { return 9026 };
    let pg = int_val(d, Some(s), "pgSz", "w");
    let mar = d.find_child(s, ns::W, "pgMar");
    let side = |a: &str| mar.and_then(|m| w_attr(d, m, a)).and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(1440);
    match pg {
        Some(w) if w > 0 => (w - side("left") - side("right")).max(720),
        _ => 9026,
    }
}

/// The table style to use: the document's most-used one, else `TableGrid`, else none.
fn table_style(doc: &WordDoc, part: PartIdx) -> Option<String> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let d = doc.part(part);
    for t in d.descendants(d.root()).filter(|&c| is_w(d, c, "tbl")) {
        if let Some(s) = Table(t).style_id(d) {
            *counts.entry(s).or_default() += 1;
        }
    }
    if let Some((s, _)) = counts.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))) {
        return Some(s);
    }
    let sp = doc.styles_part()?;
    let s = doc.part(sp);
    let styles: Vec<NodeId> = s.child_elements(s.root()).filter(|&c| is_w(s, c, "style")).collect();
    if styles.iter().any(|&c| w_attr(s, c, "styleId").as_deref() == Some("TableGrid")) {
        return Some("TableGrid".into());
    }
    None
}

/// A paragraph style id by id or (case-insensitive) name.
fn find_style(doc: &WordDoc, want: &str) -> Option<String> {
    let sp = doc.styles_part()?;
    let s = doc.part(sp);
    let mut by_name = None;
    for c in s.child_elements(s.root()).filter(|&c| is_w(s, c, "style")) {
        if w_attr(s, c, "type").as_deref().is_some_and(|t| t != "paragraph") {
            continue;
        }
        let id = w_attr(s, c, "styleId").unwrap_or_default();
        if id == want {
            return Some(id);
        }
        let name = s.find_child(c, ns::W, "name").and_then(|n| w_attr(s, n, "val")).unwrap_or_default();
        if by_name.is_none() && name.eq_ignore_ascii_case(want) {
            by_name = Some(id);
        }
    }
    by_name
}

/// `insert_table`: a `rows` × `cols` table after the anchor block.
pub fn insert_table(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    alloc_rev: &mut dyn FnMut() -> u32,
    author: &str,
    date: &str,
    args: &InsertTableArgs,
) -> Result<EditOutcome> {
    if args.rows == 0 || args.cols == 0 || args.rows > 1000 || args.cols > 63 {
        return Err(err("INVALID_OP", "rows must be 1..=1000 and cols 1..=63"));
    }
    let (part, after) = match anchors.resolve(doc, &args.after) {
        Some(Target::Block { para, .. }) => (para.part, para.node),
        Some(Target::Table { table }) => (table.part, table.node),
        _ => return Err(err("NOT_FOUND", format!("no block {}", args.after))),
    };
    // Cell paragraph style.
    let style = args.style.as_deref().unwrap_or("inherit");
    let pstyle: Option<Option<String>> = match style {
        "inherit" => None,
        "P" => Some(None),
        k if k.len() == 2 && k.starts_with('H') && k[1..].parse::<u8>().is_ok_and(|n| (1..=9).contains(&n)) => {
            let n = &k[1..];
            Some(Some(
                find_style(doc, &format!("Heading{n}"))
                    .or_else(|| find_style(doc, &format!("heading {n}")))
                    .ok_or_else(|| err("NOT_FOUND", format!("no heading {n} style")))?,
            ))
        }
        "LI" => Some(Some(
            find_style(doc, "ListParagraph")
                .or_else(|| find_style(doc, "List Paragraph"))
                .ok_or_else(|| err("NOT_FOUND", "no list paragraph style"))?,
        )),
        s => Some(Some(find_style(doc, s).ok_or_else(|| err("NOT_FOUND", format!("no paragraph style {s}")))?)),
    };
    let tstyle = table_style(doc, part);
    let track = args.guards.mode() == Mode::Track;
    let tracker = Tracker::new(anchors);
    let mut rv = Rv { author, date, ids: Vec::new() };
    let made;
    {
        let d = doc.part_mut(part);
        let wq = Wq::of(d);
        let in_cell = d.parent(after).is_some_and(|p| is_w(d, p, "tc"));
        let tw = text_width(d, after);
        let colw = (tw / args.cols as i64).max(1);
        // Cell paragraph properties template.
        let ppr_src = match &pstyle {
            None => {
                let anchor_p = if is_w(d, after, "p") { Some(after) } else { None };
                match anchor_p.and_then(|p| d.find_child(p, ns::W, "pPr")) {
                    Some(src) => {
                        Some(clean_ppr(d, src, &["numPr", "pageBreakBefore", "framePr", "keepNext", "outlineLvl"])?)
                    }
                    None => None,
                }
            }
            Some(None) => None,
            Some(Some(id)) => {
                let ppr = wq.el(d, "pPr", &[])?;
                let ps = wq.el(d, "pStyle", &[("val", id)])?;
                d.append_child(ppr, ps)?;
                Some(ppr)
            }
        };

        let data = args.data.clone().unwrap_or_default();
        if data.len() > args.rows as usize || data.iter().any(|r| r.len() > args.cols as usize) {
            return Err(err("INVALID_OP", "data has more rows or columns than the table"));
        }
        let tbl = wq.el(d, "tbl", &[])?;
        d.insert_after(after, tbl)?;
        let tblpr = wq.el(d, "tblPr", &[])?;
        d.append_child(tbl, tblpr)?;
        if let Some(s) = &tstyle {
            let x = wq.el(d, "tblStyle", &[("val", s)])?;
            d.append_child(tblpr, x)?;
        }
        let x = wq.el(d, "tblW", &[("w", "5000"), ("type", "pct")])?;
        d.append_child(tblpr, x)?;
        if tstyle.is_none() {
            let b = wq.el(d, "tblBorders", &[])?;
            for side in ["top", "left", "bottom", "right", "insideH", "insideV"] {
                let e = wq.el(d, side, &[("val", "single"), ("sz", "4"), ("space", "0"), ("color", "auto")])?;
                d.append_child(b, e)?;
            }
            d.append_child(tblpr, b)?;
        }
        let x = wq.el(
            d,
            "tblLook",
            &[
                ("val", if args.header_row == Some(true) { "04A0" } else { "0480" }),
                ("firstRow", if args.header_row == Some(true) { "1" } else { "0" }),
                ("lastRow", "0"),
                ("firstColumn", "1"),
                ("lastColumn", "0"),
                ("noHBand", "0"),
                ("noVBand", "1"),
            ],
        )?;
        d.append_child(tblpr, x)?;
        let g = wq.el(d, "tblGrid", &[])?;
        d.append_child(tbl, g)?;
        for _ in 0..args.cols {
            let gc = wq.el(d, "gridCol", &[("w", &colw.to_string())])?;
            d.append_child(g, gc)?;
        }
        for r in 0..args.rows as usize {
            let tr = wq.el(d, "tr", &[])?;
            d.append_child(tbl, tr)?;
            if args.header_row == Some(true) && r == 0 {
                let trpr = trpr_of(d, &wq, tr)?;
                let h = wq.el(d, "tblHeader", &[])?;
                d.append_child(trpr, h)?;
            }
            if track {
                let id = rv.id(alloc_rev);
                mark_row(d, &wq, tr, "ins", id, author, date)?;
            }
            for c in 0..args.cols as usize {
                let tc = wq.el(d, "tc", &[])?;
                d.append_child(tr, tc)?;
                let tcpr = wq.el(d, "tcPr", &[])?;
                d.append_child(tc, tcpr)?;
                set_tcw(d, &wq, tcpr, colw)?;
                let text = data.get(r).and_then(|row| row.get(c)).map(String::as_str).unwrap_or("");
                new_paras(d, &wq, Place::Append(tc), ppr_src, None, text, track.then_some(&mut rv), alloc_rev)?;
            }
        }
        if let Some(p) = ppr_src {
            d.remove(p)?;
        }
        // A paragraph after the table when nothing (or another table / the body's section
        // properties) follows: Word needs it, and adjacent tables would fuse.
        let next = d.next_sibling(tbl).filter(|&n| d.is_element_node(n));
        let needs = match next {
            None => true,
            Some(n) => is_w(d, n, "tbl") || is_w(d, n, "sectPr"),
        } || (in_cell && d.child_elements(d.parent(tbl).unwrap_or(tbl)).last() == Some(tbl));
        let mut all = paras_in(d, tbl);
        if needs {
            let p = wq.el(d, "p", &[])?;
            d.insert_after(tbl, p)?;
            if track {
                let id = rv.id(alloc_rev);
                mark_paragraph(d, &wq, p, "ins", id, author, date)?;
            }
            all.push(p);
        }
        made = all;
    }
    stamp_para_ids(doc, part, &made)?;
    let mut out = tracker.finish(doc, anchors, rv.ids);
    out.changed.clear();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::id_allocator;
    use super::super::review::{ReviewArgs, accept, reject};
    use super::super::test_util::{body_xml, doc};
    use super::*;

    const T: &str = r#"<w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="3000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>after</w:t></w:r></w:p>"#;

    fn all(accept_it: bool, wd: &mut WordDoc, an: &mut Anchors) {
        let mut alloc = id_allocator(wd);
        let a = ReviewArgs { all: Some(true), ..Default::default() };
        if accept_it {
            accept(wd, an, &mut alloc, "x", "d", &a).unwrap();
        } else {
            reject(wd, an, &mut alloc, "x", "d", &a).unwrap();
        }
    }

    fn texts(wd: &WordDoc) -> String {
        let d = wd.part(wd.main_part());
        text::text(d, d.root(), TextMode::AsIs)
    }

    fn direct() -> Guards {
        Guards { mode: Some(Mode::Direct), ..Default::default() }
    }

    #[test]
    fn insert_row_tracked_then_accept_reject() {
        for acc in [true, false] {
            let mut wd = doc(T);
            let mut an = Anchors::mint(&wd);
            let mut alloc = id_allocator(&wd);
            let args = InsertRowArgs {
                after: Some("t1.r1".into()),
                cells: Some(vec!["x".into(), "y".into()]),
                ..Default::default()
            };
            let out = insert_row(&mut wd, &mut an, &mut alloc, "Ann", "d", &args).unwrap();
            assert_eq!(out.inserted.len(), 2, "{out:?}");
            let x = body_xml(&wd);
            assert!(x.contains(r#"<w:tr><w:trPr><w:ins w:id="0" w:author="Ann" w:date="d"/></w:trPr><w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:pPr><w:jc w:val="center"/><w:rPr><w:ins "#), "{x}");
            assert!(
                x.contains(
                    r#"<w:ins w:id="2" w:author="Ann" w:date="d"><w:r><w:rPr><w:b/></w:rPr><w:t>x</w:t></w:r></w:ins>"#
                ),
                "{x}"
            );
            all(acc, &mut wd, &mut an);
            let t = texts(&wd);
            if acc {
                assert_eq!(t, "A1\nB1\nx\ny\nA2\nB2\nafter");
                assert!(!body_xml(&wd).contains("w:ins"));
            } else {
                assert_eq!(t, "A1\nB1\nA2\nB2\nafter");
                assert_eq!(body_xml(&wd), T);
            }
        }
    }

    #[test]
    fn delete_row_tracked_and_direct() {
        let mut wd = doc(T);
        let mut an = Anchors::mint(&wd);
        let mut alloc = id_allocator(&wd);
        let out = delete_row(
            &mut wd,
            &mut an,
            &mut alloc,
            "Ann",
            "d",
            &DeleteRowArgs { row: "t1.r2".into(), ..Default::default() },
        )
        .unwrap();
        assert_eq!(out.changed.len(), 2);
        assert!(body_xml(&wd).contains(r#"<w:tr><w:trPr><w:del w:id="0""#));
        all(false, &mut wd, &mut an);
        assert_eq!(body_xml(&wd), T);
        let mut alloc = id_allocator(&wd);
        delete_row(
            &mut wd,
            &mut an,
            &mut alloc,
            "Ann",
            "d",
            &DeleteRowArgs { row: "t1.r2".into(), ..Default::default() },
        )
        .unwrap();
        all(true, &mut wd, &mut an);
        assert_eq!(texts(&wd), "A1\nB1\nafter");

        let mut wd = doc(T);
        let mut an = Anchors::mint(&wd);
        let out = delete_row(
            &mut wd,
            &mut an,
            &mut alloc,
            "A",
            "d",
            &DeleteRowArgs { guards: direct(), row: "t1.r1".into() },
        )
        .unwrap();
        assert_eq!(out.deleted.len(), 2, "{out:?}");
        assert_eq!(texts(&wd), "A2\nB2\nafter");
    }

    #[test]
    fn set_cell_tracked_and_direct() {
        for acc in [true, false] {
            let mut wd = doc(T);
            let mut an = Anchors::mint(&wd);
            let mut alloc = id_allocator(&wd);
            let args = SetCellArgs { cell: "t1.r1.c1".into(), text: "New\nlines".into(), ..Default::default() };
            let out = set_cell(&mut wd, &mut an, &mut alloc, "Ann", "d", &args).unwrap();
            assert_eq!(out.changed.len(), 1);
            assert_eq!(out.inserted.len(), 1);
            all(acc, &mut wd, &mut an);
            if acc {
                assert_eq!(texts(&wd), "New\nlines\nB1\nA2\nB2\nafter");
                let x = body_xml(&wd);
                assert!(x.contains(r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>New</w:t></w:r></w:p>"#), "{x}");
            } else {
                assert_eq!(body_xml(&wd), T);
            }
        }
        let mut wd = doc(T);
        let mut an = Anchors::mint(&wd);
        let mut alloc = id_allocator(&wd);
        let args = SetCellArgs { guards: direct(), cell: "t1.r1.c1".into(), text: " Z ".into() };
        set_cell(&mut wd, &mut an, &mut alloc, "A", "d", &args).unwrap();
        assert!(body_xml(&wd).contains(r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve"> Z </w:t></w:r></w:p>"#));
        let out = set_cell(&mut wd, &mut an, &mut alloc, "A", "d", &args).unwrap();
        assert_eq!(out, EditOutcome::default());
    }

    #[test]
    fn columns() {
        let mut wd = doc(T);
        let mut an = Anchors::mint(&wd);
        let mut alloc = id_allocator(&wd);
        let args = InsertColumnArgs { table: "t1".into(), at: 2, cells: Some(vec!["m1".into()]), ..Default::default() };
        let e = insert_column(&mut wd, &mut an, &mut alloc, "A", "d", &args).unwrap_err();
        assert!(e.to_string().starts_with("CONFIRM_REQUIRED: beta"), "{e}");
        for acc in [true, false] {
            let mut wd = doc(T);
            let mut an = Anchors::mint(&wd);
            let mut alloc = id_allocator(&wd);
            let args = InsertColumnArgs { confirm: Some(true), ..args.clone() };
            insert_column(&mut wd, &mut an, &mut alloc, "A", "d", &args).unwrap();
            let x = body_xml(&wd);
            assert!(x.contains(r#"<w:gridCol w:w="2000"/><w:gridCol w:w="2500"/><w:gridCol w:w="3000"/><w:tblGridChange w:id="0"><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="3000"/></w:tblGrid></w:tblGridChange>"#), "{x}");
            assert!(x.contains("<w:cellIns "), "{x}");
            all(acc, &mut wd, &mut an);
            if acc {
                assert_eq!(texts(&wd), "A1\nm1\nB1\nA2\n\nB2\nafter");
                assert!(!body_xml(&wd).contains("cellIns"));
            } else {
                assert_eq!(body_xml(&wd), T);
            }
        }
        // Tracked delete, then accept: cells and grid column gone.
        let mut wd = doc(T);
        let mut an = Anchors::mint(&wd);
        let mut alloc = id_allocator(&wd);
        let args = DeleteColumnArgs { table: "t1".into(), column: 1, confirm: Some(true), ..Default::default() };
        delete_column(&mut wd, &mut an, &mut alloc, "A", "d", &args).unwrap();
        all(true, &mut wd, &mut an);
        let x = body_xml(&wd);
        assert_eq!(texts(&wd), "B1\nB2\nafter");
        assert!(x.contains(r#"<w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid>"#), "{x}");
        // Direct delete.
        let mut wd = doc(T);
        let mut an = Anchors::mint(&wd);
        let args = DeleteColumnArgs { guards: direct(), table: "t1".into(), column: 2, confirm: None };
        delete_column(&mut wd, &mut an, &mut alloc, "A", "d", &args).unwrap();
        assert_eq!(texts(&wd), "A1\nA2\nafter");
        assert!(body_xml(&wd).contains(r#"<w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>"#));
    }

    #[test]
    fn merge_and_split() {
        let mut wd = doc(T);
        let mut an = Anchors::mint(&wd);
        let mut alloc = id_allocator(&wd);
        let m = MergeCellsArgs { from: "t1.r1.c1".into(), to: "t1.r1.c2".into(), ..Default::default() };
        let e = merge_cells(&mut wd, &mut an, &mut alloc, "A", "d", &m).unwrap_err();
        assert!(e.to_string().starts_with("UNSUPPORTED: direct_only"), "{e}");
        let m = MergeCellsArgs { guards: direct(), ..m };
        merge_cells(&mut wd, &mut an, &mut alloc, "A", "d", &m).unwrap();
        let x = body_xml(&wd);
        assert!(x.contains(r#"<w:tcPr><w:tcW w:w="5000" w:type="dxa"/><w:gridSpan w:val="2"/></w:tcPr>"#), "{x}");
        assert_eq!(texts(&wd), "A1\nB1\nA2\nB2\nafter");
        // Split it back.
        let s = SplitCellArgs { guards: direct(), cell: "t1.r1.c1".into(), cols: Some(2), rows: None };
        split_cell(&mut wd, &mut an, &mut alloc, "A", "d", &s).unwrap();
        let d = wd.part(wd.main_part());
        let tbl = d.descendants(d.root()).find(|&c| is_w(d, c, "tbl")).unwrap();
        assert_eq!(Row(Table(tbl).rows(d)[0].0).cells(d).len(), 2);

        // Vertical merge of a column.
        let mut wd = doc(T);
        let mut an = Anchors::mint(&wd);
        let m = MergeCellsArgs { guards: direct(), from: "t1.r1.c2".into(), to: "t1.r2.c2".into() };
        merge_cells(&mut wd, &mut an, &mut alloc, "A", "d", &m).unwrap();
        let x = body_xml(&wd);
        assert!(x.contains(r#"<w:vMerge w:val="restart"/>"#) && x.contains("<w:vMerge/>"), "{x}");
        assert_eq!(texts(&wd), "A1\nB1\nB2\nA2\n\nafter");
        // Split rows: unmerge.
        let s = SplitCellArgs { guards: direct(), cell: "t1.r1.c2".into(), rows: Some(2), cols: None };
        split_cell(&mut wd, &mut an, &mut alloc, "A", "d", &s).unwrap();
        assert!(!body_xml(&wd).contains("vMerge"));

        // Split a single column cell into two: grid grows, the other row spans.
        let mut wd = doc(T);
        let mut an = Anchors::mint(&wd);
        let s = SplitCellArgs { guards: direct(), cell: "t1.r2.c1".into(), cols: Some(2), rows: None };
        split_cell(&mut wd, &mut an, &mut alloc, "A", "d", &s).unwrap();
        let x = body_xml(&wd);
        assert!(x.contains(r#"<w:gridCol w:w="1000"/><w:gridCol w:w="1000"/><w:gridCol w:w="3000"/>"#), "{x}");
        assert!(x.contains(r#"<w:gridSpan w:val="2"/>"#), "{x}");
        // Split a cell into rows: a new row, the neighbour merges down.
        let s = SplitCellArgs { guards: direct(), cell: "t1.r1.c2".into(), rows: Some(2), cols: None };
        split_cell(&mut wd, &mut an, &mut alloc, "A", "d", &s).unwrap();
        let d = wd.part(wd.main_part());
        let tbl = d.descendants(d.root()).find(|&c| is_w(d, c, "tbl")).unwrap();
        assert_eq!(Table(tbl).rows(d).len(), 3);
    }

    #[test]
    fn insert_table_tracked() {
        for acc in [true, false] {
            let mut wd = doc(T);
            let mut an = Anchors::mint(&wd);
            let h = an.handles(0).last().unwrap().0.to_owned();
            let mut alloc = id_allocator(&wd);
            let args = InsertTableArgs {
                after: h,
                rows: 2,
                cols: 3,
                header_row: Some(true),
                data: Some(vec![vec!["h1".into(), "h2".into()], vec!["a".into()]]),
                ..Default::default()
            };
            let out = insert_table(&mut wd, &mut an, &mut alloc, "A", "d", &args).unwrap();
            assert!(out.inserted.iter().any(|h| h == "t2"), "{out:?}");
            let x = body_xml(&wd);
            assert!(x.contains(r#"<w:tblStyle w:val="TableGrid"/><w:tblW w:w="5000" w:type="pct"/>"#), "{x}");
            assert!(x.contains(r#"<w:trPr><w:tblHeader/><w:ins "#), "{x}");
            assert!(
                x.ends_with(
                    r#"</w:tbl><w:p><w:pPr><w:rPr><w:ins w:id="11" w:author="A" w:date="d"/></w:rPr></w:pPr></w:p>"#
                ),
                "{x}"
            );
            all(acc, &mut wd, &mut an);
            if acc {
                assert_eq!(texts(&wd), "A1\nB1\nA2\nB2\nafter\nh1\nh2\n\na\n\n\n");
            } else {
                assert_eq!(body_xml(&wd), T);
            }
        }
    }
}
