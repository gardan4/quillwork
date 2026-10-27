//! Block ops: `insert_paragraph`, `delete_block`, `move_block`, `merge_paragraphs`,
//! `set_paragraph`, `insert_break`.
//!
//! New paragraphs clone the properties of the paragraph they are inserted next to (minus section
//! breaks and revision marks) and take the run formatting of its neighbouring text. In track mode
//! deletions keep the text as `w:del` runs with a deleted paragraph mark, insertions are wrapped
//! in `w:ins` with an inserted mark, and property changes carry a `w:pPrChange` snapshot.

use serde::Deserialize;
use serde_json::Value;

use super::text::{delete_runs, in_locked_control, locked_error, next_element, refresh_ids};
use super::{Cx, OpError, OpResult, Resolved, Tk, args, resolve};
use crate::anchors::Target;
use crate::edit::{EditOutcome, Tracker, has_content, is_ignored_branch, walk};
use crate::track::write::{self as tw, PPR_ORDER, RPR_ORDER, is_w, w_attr, w_child, w_local};
use crate::wml::style::{StyleKind, StyleSheet};
use crate::wml::view::{Paragraph, Sdt};
use crate::xml::{Document, NodeId};

// ---------------------------------------------------------------------- shared helpers

/// `CT_TrPr`: row properties, then the row's revision markers.
const TRPR_ORDER: &[&str] = &[
    "cnfStyle",
    "divId",
    "gridBefore",
    "gridAfter",
    "wBefore",
    "wAfter",
    "cantSplit",
    "trHeight",
    "tblHeader",
    "tblCellSpacing",
    "jc",
    "hidden",
    "ins",
    "del",
    "trPrChange",
];

/// `CT_Row`.
const TR_ORDER: &[&str] = &["tblPrEx", "trPr", "tc"];

/// `CT_SectPr`.
const SECT_ORDER: &[&str] = &[
    "headerReference",
    "footerReference",
    "footnotePr",
    "endnotePr",
    "type",
    "pgSz",
    "pgMar",
    "paperSrc",
    "pgBorders",
    "lnNumType",
    "pgNumType",
    "cols",
    "formProt",
    "vAlign",
    "noEndnote",
    "titlePg",
    "textDirection",
    "bidi",
    "rtlGutter",
    "docGrid",
    "printerSettings",
    "sectPrChange",
];

/// `CT_NumPr`.
pub(crate) const NUMPR_ORDER: &[&str] = &["ilvl", "numId", "numberingChange", "ins"];

/// Range markup that must survive when the block holding it goes.
const RANGE_MARKUP: &[&str] =
    &["bookmarkStart", "bookmarkEnd", "commentRangeStart", "commentRangeEnd", "permStart", "permEnd"];

/// The runs of a block that are not already deleted (paragraph-mark and nested story content
/// excluded).
pub(crate) fn block_runs(d: &Document, node: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    if is_w(d, node, "r") {
        return vec![node];
    }
    walk(d, node, |c| {
        if is_ignored_branch(d, c) {
            return false;
        }
        match w_local(d, c) {
            Some("r") => {
                out.push(c);
                false
            }
            Some(
                "pPr" | "rPr" | "txbxContent" | "sectPr" | "tblPr" | "trPr" | "tcPr" | "tblGrid" | "del" | "moveFrom"
                | "sdtPr" | "sdtEndPr",
            ) => false,
            _ => true,
        }
    });
    out
}

/// Runs grouped by parent, each group a run of consecutive element siblings.
fn sibling_groups(d: &Document, runs: &[NodeId]) -> Vec<Vec<NodeId>> {
    let mut groups: Vec<Vec<NodeId>> = Vec::new();
    for &r in runs {
        match groups.last_mut() {
            Some(g) if g.last().and_then(|&l| next_element(d, l)) == Some(r) => g.push(r),
            _ => groups.push(vec![r]),
        }
    }
    groups
}

fn is_block(d: &Document, n: NodeId) -> bool {
    matches!(w_local(d, n), Some("p" | "tbl" | "sdt" | "customXml"))
}

/// Is `n` the last block of its container (body, cell, content control, …)?
fn last_in_container(d: &Document, n: NodeId) -> bool {
    let mut cur = next_element(d, n);
    while let Some(c) = cur {
        if is_block(d, c) {
            return false;
        }
        cur = next_element(d, c);
    }
    true
}

/// The next / previous paragraph sibling.
fn sibling_para(d: &Document, n: NodeId, forward: bool) -> Option<NodeId> {
    let mut cur = if forward { d.next_sibling(n) } else { d.prev_sibling(n) };
    while let Some(c) = cur {
        if is_w(d, c, "p") {
            return Some(c);
        }
        cur = if forward { d.next_sibling(c) } else { d.prev_sibling(c) };
    }
    None
}

fn own_mark_ins(d: &Document, tk: &Tk, p: NodeId) -> bool {
    tw::mark_revision(d, p, "ins").is_some_and(|m| tk.is_own(d, m))
}

fn has_section(d: &Document, p: NodeId) -> bool {
    w_child(d, p, "pPr").is_some_and(|ppr| w_child(d, ppr, "sectPr").is_some())
}

fn locked_inside(d: &Document, n: NodeId) -> bool {
    in_locked_control(d, n) || d.descendants(n).any(|c| is_w(d, c, "sdt") && Sdt(c).content_locked(d))
}

/// The block (paragraph, table, content control, custom XML) after / before `n` among its siblings.
fn sibling_block(d: &Document, n: NodeId, forward: bool) -> Option<NodeId> {
    let mut cur = if forward { d.next_sibling(n) } else { d.prev_sibling(n) };
    while let Some(c) = cur {
        if d.is_element_node(c) && is_block(d, c) {
            return Some(c);
        }
        cur = if forward { d.next_sibling(c) } else { d.prev_sibling(c) };
    }
    None
}

/// Does removing `p`'s mark join it with the next paragraph? Not when a table, a block content
/// control or the end of the container follows: Word (and every consumer we test against) keeps
/// that paragraph, so a mark revision there would leave an empty paragraph behind (DESIGN §7.3).
fn mark_joins(d: &Document, p: NodeId) -> bool {
    sibling_block(d, p, true).is_some_and(|b| is_w(d, b, "p"))
}

/// The paragraph whose mark carries a mark revision `kind` ("ins" / "del") in place of `p`'s,
/// when `p`'s mark can't (see [`mark_joins`]): the nearest paragraph before `p`, past paragraphs
/// whose mark already carries `kind` (they join the same chain). None when a table, a content
/// control, a section break or the start of the container comes first.
fn stand_in(d: &Document, p: NodeId, kind: &str) -> Option<NodeId> {
    let mut cur = sibling_block(d, p, false);
    while let Some(b) = cur {
        if !is_w(d, b, "p") || has_section(d, b) {
            return None;
        }
        if tw::mark_revision(d, b, kind).is_none() {
            return Some(b);
        }
        cur = sibling_block(d, b, false);
    }
    None
}

/// Track `p`'s mark as inserted; when it can't join the next paragraph (see [`mark_joins`]), the
/// previous paragraph's mark is inserted instead, so rejecting joins that paragraph with `p`'s
/// emptied remains. Returns that paragraph when it took the mark. With no previous paragraph
/// `p`'s own mark is marked (rejecting then leaves an empty paragraph, as in Word).
fn mark_inserted(d: &mut Document, tk: &mut Tk, p: NodeId) -> crate::Result<Option<NodeId>> {
    let target = if mark_joins(d, p) { p } else { stand_in(d, p, "ins").unwrap_or(p) };
    if tw::mark_revision(d, target, "ins").is_none() {
        let id = tk.rev();
        tw::mark_paragraph(d, target, "ins", id, &tk.stamp)?;
    }
    Ok((target != p).then_some(target))
}

/// Tracked delete of one paragraph: its live runs become `w:del` and its mark is deleted. A
/// paragraph this transaction inserted and that holds nothing else goes. When the mark can't
/// join a next paragraph (a table or the end of the container follows), it stays and the
/// previous paragraph's mark is deleted instead; that paragraph is returned (it changed too).
/// With no previous paragraph an empty paragraph is left, as Word leaves it.
fn delete_para_tracked(d: &mut Document, tk: &mut Tk, p: NodeId) -> crate::Result<Option<NodeId>> {
    let runs = block_runs(d, p);
    delete_runs(d, tk, p, &runs, false)?;
    if own_mark_ins(d, tk, p) && !has_content(d, p) && !last_in_container(d, p) {
        d.remove(p)?;
        return Ok(None);
    }
    let target = if mark_joins(d, p) { Some(p) } else { stand_in(d, p, "del") };
    let Some(q) = target else { return Ok(None) };
    if tw::mark_revision(d, q, "del").is_none() {
        let id = tk.rev();
        tw::mark_paragraph(d, q, "del", id, &tk.stamp)?;
    }
    Ok((q != p).then_some(q))
}

/// Tracked delete of a paragraph or table. Returns the paragraphs outside `node` whose mark it
/// deleted (see [`delete_para_tracked`]).
pub(crate) fn delete_block_tracked(d: &mut Document, tk: &mut Tk, node: NodeId) -> crate::Result<Vec<NodeId>> {
    let mut outside = Vec::new();
    if is_w(d, node, "tbl") {
        let rows: Vec<NodeId> = d.descendants(node).filter(|&c| is_w(d, c, "tr")).collect();
        let all_own = !rows.is_empty()
            && rows.iter().all(|&tr| {
                w_child(d, tr, "trPr").and_then(|pr| w_child(d, pr, "ins")).is_some_and(|i| tk.is_own(d, i))
            });
        if all_own && tk.track() {
            d.remove(node)?;
            return Ok(outside);
        }
        for tr in rows {
            let trpr = tw::ensure_child(d, tr, "trPr", TR_ORDER)?;
            if w_child(d, trpr, "del").is_none() {
                let del = tw::revision_el(d, "del", tk.rev(), &tk.stamp)?;
                tw::insert_ordered(d, trpr, del, TRPR_ORDER)?;
            }
        }
        let paras: Vec<NodeId> = d.descendants(node).filter(|&c| is_w(d, c, "p")).collect();
        for p in paras {
            delete_para_tracked(d, tk, p)?;
        }
        return Ok(outside);
    }
    let paras: Vec<NodeId> =
        if is_w(d, node, "p") { vec![node] } else { d.descendants(node).filter(|&c| is_w(d, c, "p")).collect() };
    for p in paras {
        if let Some(q) = delete_para_tracked(d, tk, p)?
            && !d.ancestors(q).any(|a| a == node)
            && q != node
        {
            outside.push(q);
        }
    }
    Ok(outside)
}

/// Mark a (moved) paragraph or table as inserted by this change set: earlier revisions in it are
/// settled (deleted text dropped, other insertions unwrapped) and its runs wrapped in `w:ins`.
/// Returns the paragraph outside `node` whose mark it marked (see [`mark_inserted`]).
fn insert_block_tracked(d: &mut Document, tk: &mut Tk, node: NodeId) -> crate::Result<Vec<NodeId>> {
    let mut outside = Vec::new();
    let all: Vec<NodeId> = std::iter::once(node).chain(d.descendants(node)).collect();
    for c in all {
        if !d.is_element_node(c) || !crate::edit::attached(d, c) {
            continue;
        }
        let in_mark = d.parent(c).is_some_and(|par| is_w(d, par, "rPr"));
        match w_local(d, c) {
            Some("del" | "moveFrom") if !in_mark && !d.parent(c).is_some_and(|par| is_w(d, par, "trPr")) => {
                d.remove(c)?
            }
            Some("ins" | "moveTo") if !in_mark && !d.parent(c).is_some_and(|par| is_w(d, par, "trPr")) => {
                crate::edit::unwrap(d, c)?
            }
            Some("ins" | "del" | "moveFrom" | "moveTo") => d.remove(c)?,
            _ => {}
        }
    }
    if is_w(d, node, "tbl") {
        let rows: Vec<NodeId> = d.descendants(node).filter(|&c| is_w(d, c, "tr")).collect();
        for tr in rows {
            let trpr = tw::ensure_child(d, tr, "trPr", TR_ORDER)?;
            let ins = tw::revision_el(d, "ins", tk.rev(), &tk.stamp)?;
            tw::insert_ordered(d, trpr, ins, TRPR_ORDER)?;
        }
    }
    let paras: Vec<NodeId> =
        if is_w(d, node, "p") { vec![node] } else { d.descendants(node).filter(|&c| is_w(d, c, "p")).collect() };
    for p in paras {
        let runs = block_runs(d, p);
        for g in sibling_groups(d, &runs) {
            let ins = tw::revision_el(d, "ins", tk.rev(), &tk.stamp)?;
            tw::wrap(d, &g, ins)?;
        }
        if let Some(q) = mark_inserted(d, tk, p)?
            && !d.ancestors(q).any(|a| a == node)
            && q != node
        {
            outside.push(q);
        }
    }
    Ok(outside)
}

/// Insert `nodes` at the front of paragraph `p` (after its `w:pPr`).
fn put_front(d: &mut Document, p: NodeId, nodes: &[NodeId]) -> crate::Result<()> {
    let first = d.child_elements(p).find(|&c| !is_w(d, c, "pPr"));
    for &n in nodes {
        match first {
            Some(f) => d.insert_before(f, n)?,
            None => d.append_child(p, n)?,
        }
    }
    Ok(())
}

/// Range markup and comment-reference runs below `node`.
fn salvageable(d: &Document, node: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    walk(d, node, |c| {
        let local = w_local(d, c).unwrap_or("");
        if RANGE_MARKUP.contains(&local) {
            out.push(c);
            return false;
        }
        if local == "r" {
            if d.child_elements(c).any(|k| is_w(d, k, "commentReference")) {
                out.push(c);
            }
            return false;
        }
        true
    });
    out
}

/// Direct delete: the block goes; its bookmarks, comment ranges and comment references move to a
/// neighbouring paragraph. The last paragraph of a container (or one carrying a section break) is
/// emptied instead.
fn delete_block_direct(d: &mut Document, node: NodeId) -> crate::Result<()> {
    if is_w(d, node, "p") && (last_in_container(d, node) || has_section(d, node)) {
        let keep = salvageable(d, node);
        for c in d.children(node).collect::<Vec<_>>() {
            if d.is_element_node(c) && is_w(d, c, "pPr") {
                continue;
            }
            d.remove(c)?;
        }
        for k in keep {
            d.append_child(node, k)?;
        }
        return Ok(());
    }
    let keep = salvageable(d, node);
    let parent = d.parent(node);
    if !keep.is_empty() {
        let (next, prev) = (sibling_para(d, node, true), sibling_para(d, node, false));
        let mut moved = Vec::new();
        for k in keep {
            d.remove(k)?;
            moved.push(k);
        }
        match (next, prev) {
            (Some(n), _) => put_front(d, n, &moved)?,
            (None, Some(p)) => {
                for k in moved {
                    d.append_child(p, k)?;
                }
            }
            _ => {}
        }
    }
    d.remove(node)?;
    if let Some(par) = parent {
        ensure_cell_ends_in_p(d, par)?;
    }
    Ok(())
}

/// A table cell must end with a paragraph.
fn ensure_cell_ends_in_p(d: &mut Document, container: NodeId) -> crate::Result<()> {
    if !is_w(d, container, "tc") {
        return Ok(());
    }
    let last = d.child_elements(container).filter(|&c| is_block(d, c)).last();
    if !last.is_some_and(|l| is_w(d, l, "p")) {
        let p = tw::new_w(d, "p")?;
        d.append_child(container, p)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------- styles

/// Style id for a `style` argument: `inherit` (None), a projection kind (P, H1–H9, LI, TITLE),
/// a paragraph style id or a style name (localised built-in names included).
pub(crate) fn resolve_style(sheet: Option<&StyleSheet>, value: &str, path: String) -> OpResult<Option<String>> {
    let v = value.trim();
    if v.eq_ignore_ascii_case("inherit") {
        return Ok(None);
    }
    let missing =
        || OpError { path: Some(path.clone()), ..OpError::not_found("style", format!("no paragraph style {v:?}")) };
    let Some(sheet) = sheet else {
        return if v == "P" { Ok(Some(String::new())) } else { Err(missing()) };
    };
    let by_name = |n: &str| sheet.style_by_name_of_kind(n, StyleKind::Paragraph).map(|s| s.id().to_owned());
    let kind = match v {
        "P" => return Ok(Some(sheet.default_paragraph_style_id().unwrap_or("Normal").to_owned())),
        "LI" => by_name("List Paragraph"),
        "TITLE" => by_name("Title"),
        _ => match v.strip_prefix('H').and_then(|n| n.parse::<u8>().ok()) {
            Some(n) if (1..=9).contains(&n) => by_name(&format!("heading {n}")),
            _ => None,
        },
    };
    if let Some(id) = kind {
        return Ok(Some(id));
    }
    if let Some(s) = sheet.style_by_id(v)
        && s.kind() == StyleKind::Paragraph
    {
        return Ok(Some(s.id().to_owned()));
    }
    by_name(v).map(Some).ok_or_else(missing)
}

/// Set (or, for the default paragraph style, remove) `w:pStyle`.
pub(crate) fn set_style(d: &mut Document, sheet: Option<&StyleSheet>, p: NodeId, id: &str) -> crate::Result<()> {
    let ppr = tw::ensure_ppr(d, p)?;
    let default = sheet.and_then(|s| s.default_paragraph_style_id()).unwrap_or("Normal");
    if id.is_empty() || id == default {
        if let Some(ps) = w_child(d, ppr, "pStyle") {
            d.remove(ps)?;
        }
        return Ok(());
    }
    let ps = tw::ensure_child(d, ppr, "pStyle", PPR_ORDER)?;
    tw::set_w(d, ps, "val", id)
}

/// Does the paragraph style `id` (or its base styles) carry numbering?
pub(crate) fn style_numbered(sheet: Option<&StyleSheet>, id: Option<&str>) -> bool {
    let Some(sheet) = sheet else { return false };
    let id = match id {
        Some(i) => i.to_owned(),
        None => sheet.default_paragraph_style_id().unwrap_or("Normal").to_owned(),
    };
    sheet.style_chain(&id).iter().any(|s| s.num_id().is_some_and(|n| n != 0))
}

fn is_heading(sheet: Option<&StyleSheet>, d: &Document, p: NodeId) -> bool {
    if let Some(ppr) = w_child(d, p, "pPr")
        && let Some(o) = w_child(d, ppr, "outlineLvl")
        && w_attr(d, o, "val").and_then(|v| v.parse::<i32>().ok()).is_some_and(|v| v < 9)
    {
        return true;
    }
    let Some(sheet) = sheet else { return false };
    let Some(id) = Paragraph(p).style_id(d) else { return false };
    let chain = sheet.style_chain(&id);
    let named = chain.first().and_then(|s| s.name()).is_some_and(|n| {
        let n = n.to_ascii_lowercase();
        n.starts_with("heading") || n == "title" || n == "subtitle"
    });
    named || chain.iter().any(|s| s.outline_level().is_some_and(|l| l < 9))
}

// ---------------------------------------------------------------------- insert_paragraph

#[derive(Deserialize)]
struct InsertArgs {
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    before: Option<String>,
    text: String,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    list: Option<Value>,
}

pub(crate) enum ListArg {
    Continue,
    None,
    Level(u32),
}

fn list_arg(v: Option<&Value>, path: String) -> OpResult<ListArg> {
    match v {
        None | Some(Value::Null) => Ok(ListArg::Continue),
        Some(Value::String(s)) if s == "continue" => Ok(ListArg::Continue),
        Some(Value::String(s)) if s == "none" => Ok(ListArg::None),
        Some(Value::Object(o)) => match o.get("level").and_then(Value::as_u64) {
            Some(l) if l <= 8 && o.len() == 1 => Ok(ListArg::Level(l as u32)),
            _ => Err(OpError::invalid(path, "list is \"continue\", \"none\" or {level: 0-8}")),
        },
        _ => Err(OpError::invalid(path, "list is \"continue\", \"none\" or {level: 0-8}")),
    }
}

/// A detached `w:pPr` copy fit for a new paragraph (no section break, no revision marks).
fn template_ppr(d: &mut Document, template: Option<NodeId>) -> crate::Result<Option<NodeId>> {
    let Some(src) = template.and_then(|t| w_child(d, t, "pPr")) else { return Ok(None) };
    let copy = d.clone_subtree(src)?;
    for c in tw::elements(d, copy) {
        if matches!(w_local(d, c), Some("sectPr" | "pPrChange")) {
            d.remove(c)?;
        }
    }
    if let Some(mr) = w_child(d, copy, "rPr") {
        for c in tw::elements(d, mr) {
            if matches!(w_local(d, c), Some("ins" | "del" | "moveFrom" | "moveTo" | "rPrChange")) {
                d.remove(c)?;
            }
        }
    }
    Ok(Some(copy))
}

/// The run properties new text next to `template` takes: its last (or first) text run's.
fn neighbour_rpr(d: &Document, template: Option<NodeId>, last: bool) -> Option<NodeId> {
    let t = template?;
    let runs: Vec<NodeId> =
        block_runs(d, t).into_iter().filter(|&r| d.child_elements(r).any(|c| is_w(d, c, "t"))).collect();
    let r = if last { runs.last() } else { runs.first() };
    match r {
        Some(&r) => w_child(d, r, "rPr"),
        None => tw::mark_rpr(d, t),
    }
}

/// Keep only the script/size/language properties of a run's `rPr` (new text in another style).
fn keep_font_only(d: &mut Document, rpr: NodeId) -> crate::Result<()> {
    for c in tw::elements(d, rpr) {
        if !matches!(w_local(d, c), Some("rFonts" | "sz" | "szCs" | "lang")) {
            d.remove(c)?;
        }
    }
    Ok(())
}

/// `insert_paragraph {after|before, text, style?, list?}`.
pub fn insert_paragraph(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: InsertArgs = args(cx.index, op)?;
    let (key, handle, after) = match (&a.after, &a.before) {
        (Some(h), None) => ("after", h.clone(), true),
        (None, Some(h)) => ("before", h.clone(), false),
        _ => return Err(OpError::invalid(cx.path("after"), "give one of after, before")),
    };
    let r = resolve(cx.doc, cx.anchors, cx.index, key, &handle)?;
    let list = list_arg(a.list.as_ref(), cx.path("list"))?;
    let sheet = cx.doc.style_sheet()?;
    let style = match &a.style {
        Some(s) => resolve_style(sheet.as_ref(), s, cx.path("style"))?,
        None => None,
    };
    let part = r.part;
    let (anchor_node, template) = {
        let d = cx.doc.part(part);
        match r.target {
            Target::Block { para, .. } => {
                let mut tpl = para.node;
                if style.is_none() && after && is_heading(sheet.as_ref(), d, para.node) {
                    // After a heading, new text reads like the body that follows it.
                    let mut cur = sibling_para(d, para.node, true);
                    let mut found = None;
                    let mut n = 0;
                    while let Some(c) = cur {
                        if !is_heading(sheet.as_ref(), d, c) {
                            found = Some(c);
                            break;
                        }
                        n += 1;
                        if n > 50 {
                            break;
                        }
                        cur = sibling_para(d, c, true);
                    }
                    if let Some(f) = found {
                        tpl = f;
                    }
                }
                (para.node, Some(tpl))
            }
            Target::Table { table } => {
                let n = table.node;
                (n, sibling_para(d, n, !after).or_else(|| sibling_para(d, n, after)))
            }
            _ => {
                return Err(OpError::unsupported(
                    "unsupported_target",
                    format!("{handle}: insert_paragraph goes after or before a block or table; use set_cell or insert_row for cells"),
                )
                .with_anchor(&handle));
            }
        }
    };
    if in_locked_control(cx.doc.part(part), anchor_node) {
        return Err(locked_error(&handle));
    }
    let mut tracker = Tracker::new(cx.anchors);
    let rev0 = cx.tk.revisions.len();
    let template_style = template.and_then(|t| Paragraph(t).style_id(cx.doc.part(part)));
    let heading_fallback = template == Some(anchor_node)
        && style.is_none()
        && after
        && is_heading(sheet.as_ref(), cx.doc.part(part), anchor_node);
    // A heading with no body after it: new text gets the default paragraph style.
    let style = if heading_fallback { Some(String::new()) } else { style };
    let restyled = match &style {
        Some(s) => {
            let default = sheet.as_ref().and_then(|x| x.default_paragraph_style_id()).unwrap_or("Normal");
            let new_id = if s.is_empty() { default } else { s.as_str() };
            template_style.as_deref().unwrap_or(default) != new_id
        }
        None => false,
    };
    let d = cx.doc.part_mut(part);
    let src_rpr = neighbour_rpr(d, template, template != Some(anchor_node) || after);
    let lines: Vec<&str> = a.text.split('\n').collect();
    let mut prev: Option<NodeId> = None;
    let mut stand_ins = Vec::new();
    for line in lines {
        let p = tw::new_w(d, "p")?;
        // Attach first: a node appended into a detached parent stays namespace-unbound, and
        // the lookups below (pPr, rPr) would miss it.
        match (prev, after) {
            (Some(pv), _) => d.insert_after(pv, p)?,
            (None, true) => d.insert_after(anchor_node, p)?,
            (None, false) => d.insert_before(anchor_node, p)?,
        }
        prev = Some(p);
        if let Some(ppr) = template_ppr(d, template)? {
            d.append_child(p, ppr)?;
        }
        if let Some(s) = &style {
            set_style(d, sheet.as_ref(), p, s)?;
            if restyled && let Some(ppr) = w_child(d, p, "pPr") {
                for c in tw::elements(d, ppr) {
                    if matches!(w_local(d, c), Some("numPr" | "outlineLvl")) {
                        d.remove(c)?;
                    }
                }
                if let Some(mr) = w_child(d, ppr, "rPr") {
                    keep_font_only(d, mr)?;
                }
            }
        }
        apply_list(d, sheet.as_ref(), p, &list, cx.index)?;
        if !line.is_empty() {
            let run = tw::make_run(d, src_rpr, line)?;
            if restyled && let Some(rpr) = w_child(d, run, "rPr") {
                keep_font_only(d, rpr)?;
                if d.first_child(rpr).is_none() {
                    d.remove(rpr)?;
                }
            }
            if cx.tk.track() {
                let ins = tw::revision_el(d, "ins", cx.tk.rev(), &cx.tk.stamp)?;
                d.append_child(ins, run)?;
                d.append_child(p, ins)?;
            } else {
                d.append_child(p, run)?;
            }
        }
        if cx.tk.track() {
            stand_ins.extend(mark_inserted(d, cx.tk, p)?);
        }
    }
    for q in stand_ins {
        tracker.touch(cx.doc, part, q);
    }
    Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()))
}

pub(crate) fn apply_list(
    d: &mut Document,
    sheet: Option<&StyleSheet>,
    p: NodeId,
    list: &ListArg,
    index: usize,
) -> OpResult<()> {
    let style = Paragraph(p).style_id(d);
    match list {
        ListArg::Continue => {}
        ListArg::None => {
            let ppr = tw::ensure_ppr(d, p)?;
            if let Some(np) = w_child(d, ppr, "numPr") {
                d.remove(np)?;
            }
            if style_numbered(sheet, style.as_deref()) {
                let np = tw::ensure_child(d, ppr, "numPr", PPR_ORDER)?;
                let id = tw::ensure_child(d, np, "numId", NUMPR_ORDER)?;
                tw::set_w(d, id, "val", "0")?;
            }
            if w_child(d, ppr, "rPr").is_none() && d.first_child(ppr).is_none() {
                d.remove(ppr)?;
            }
        }
        ListArg::Level(l) => {
            let ppr = tw::ensure_ppr(d, p)?;
            let has = w_child(d, ppr, "numPr").is_some();
            if !has && !style_numbered(sheet, style.as_deref()) {
                return Err(OpError::invalid(
                    format!("/ops/{index}/list"),
                    "list {level} needs a list item; use set_list to start a list",
                ));
            }
            let np = tw::ensure_child(d, ppr, "numPr", PPR_ORDER)?;
            let il = tw::ensure_child(d, np, "ilvl", NUMPR_ORDER)?;
            tw::set_w(d, il, "val", &l.to_string())?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------- delete_block

#[derive(Deserialize)]
struct AnchorArgs {
    anchor: String,
}

/// The blocks (paragraphs and tables) an anchor or range covers.
fn blocks_of(cx: &Cx, r: &Resolved, key: &str) -> OpResult<Vec<NodeId>> {
    let d = cx.doc.part(r.part);
    match r.target {
        Target::Block { para, .. } if r.paras.len() > 1 => {
            let last = *r.paras.last().unwrap_or(&para.node);
            let mut out = Vec::new();
            let mut cur = Some(para.node);
            while let Some(c) = cur {
                if is_block(d, c) {
                    out.push(c);
                }
                if c == last {
                    break;
                }
                cur = next_element(d, c);
            }
            Ok(out)
        }
        Target::Block { para, .. } => Ok(vec![para.node]),
        Target::Table { table } => Ok(vec![table.node]),
        Target::Row { .. } | Target::Cell { .. } => {
            Err(OpError::invalid(cx.path(key), format!("{} is a row or cell; use delete_row or set_cell", r.handle)))
        }
        Target::Image { .. } => {
            Err(OpError::unsupported("unsupported_target", format!("{}: use delete_image for an image", r.handle)))
        }
        Target::Comment { .. } => {
            Err(OpError::invalid(cx.path(key), format!("{} is a comment; use delete_comment", r.handle)))
        }
    }
}

/// `delete_block {anchor}`: a paragraph, a table or a range of blocks, paragraph marks included.
pub fn delete_block(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: AnchorArgs = args(cx.index, op)?;
    let r = resolve(cx.doc, cx.anchors, cx.index, "anchor", &a.anchor)?;
    let blocks = blocks_of(cx, &r, "anchor")?;
    {
        let d = cx.doc.part(r.part);
        if blocks.iter().any(|&b| locked_inside(d, b)) {
            return Err(locked_error(&a.anchor));
        }
    }
    let mut tracker = Tracker::new(cx.anchors);
    for &b in &blocks {
        tracker.touch(cx.doc, r.part, b);
        tracker.touch_all_paragraphs(cx.doc, r.part, b);
    }
    let rev0 = cx.tk.revisions.len();
    let d = cx.doc.part_mut(r.part);
    let mut outside = Vec::new();
    if cx.tk.track() {
        // In document order: a paragraph before a table hands its mark deletion back past the
        // paragraphs this op already joined.
        for &b in &blocks {
            outside.extend(delete_block_tracked(d, cx.tk, b)?);
        }
    } else {
        for &b in blocks.iter().rev() {
            delete_block_direct(d, b)?;
        }
    }
    for q in outside {
        tracker.touch(cx.doc, r.part, q);
    }
    Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()))
}

// ---------------------------------------------------------------------- move_block

#[derive(Deserialize)]
struct MoveArgs {
    anchor: String,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    before: Option<String>,
}

fn block_node(cx: &Cx, r: &Resolved, key: &str) -> OpResult<NodeId> {
    match r.target {
        Target::Block { para, .. } => Ok(para.node),
        Target::Table { table } => Ok(table.node),
        _ => Err(OpError::invalid(cx.path(key), format!("{} is not a paragraph or table", r.handle))),
    }
}

/// Give every `wp:docPr` below `n` a fresh id (a copied drawing must not share it).
fn renumber_drawings(d: &mut Document, n: NodeId) -> crate::Result<()> {
    let is_docpr = |d: &Document, c: NodeId| d.is_element_node(c) && d.local_name(c) == Some("docPr");
    let mut max = 0u32;
    for c in d.descendants(d.root()) {
        if is_docpr(d, c)
            && let Some(v) = d.attr(c, "id").and_then(|v| v.parse::<u32>().ok())
        {
            max = max.max(v);
        }
    }
    let targets: Vec<NodeId> = d.descendants(n).filter(|&c| is_docpr(d, c)).collect();
    for c in targets {
        max += 1;
        d.set_attr(c, "id", &max.to_string())?;
    }
    Ok(())
}

/// Strip what a deleted copy of a moved block must not duplicate: paragraph ids, bookmarks,
/// comment ranges and references, permission ranges, content-control ids.
fn scrub_copy(d: &mut Document, copy: NodeId) -> crate::Result<()> {
    let all: Vec<NodeId> = std::iter::once(copy).chain(d.descendants(copy)).collect();
    for c in all {
        if !d.is_element_node(c) || !(c == copy || d.ancestors(c).any(|x| x == copy)) {
            continue;
        }
        let local = w_local(d, c).unwrap_or("");
        if local == "p" {
            tw::strip_para_ids(d, c)?;
        } else if RANGE_MARKUP.contains(&local)
            || (local == "r" && d.child_elements(c).any(|k| is_w(d, k, "commentReference")))
        {
            d.remove(c)?;
        } else if local == "sdtPr"
            && let Some(id) = w_child(d, c, "id")
        {
            d.remove(id)?;
        }
    }
    renumber_drawings(d, copy)
}

/// `move_block {anchor, after|before}`. Tracked: a deleted copy stays behind and the block itself
/// (which keeps its anchor) lands as an insertion. Direct: the block moves.
pub fn move_block(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: MoveArgs = args(cx.index, op)?;
    if a.anchor.contains("..") {
        return Err(OpError::invalid(cx.path("anchor"), "move_block moves one paragraph or table"));
    }
    let (key, dest_h, after) = match (&a.after, &a.before) {
        (Some(h), None) => ("after", h.clone(), true),
        (None, Some(h)) => ("before", h.clone(), false),
        _ => return Err(OpError::invalid(cx.path("after"), "give one of after, before")),
    };
    let r = resolve(cx.doc, cx.anchors, cx.index, "anchor", &a.anchor)?;
    let rd = resolve(cx.doc, cx.anchors, cx.index, key, &dest_h)?;
    let src = block_node(cx, &r, "anchor")?;
    let dest = block_node(cx, &rd, key)?;
    if r.part != rd.part {
        return Err(OpError::invalid(cx.path(key), "move_block stays within one story"));
    }
    let part = r.part;
    {
        let d = cx.doc.part(part);
        if dest == src || d.ancestors(dest).any(|x| x == src) {
            return Err(OpError::invalid(cx.path(key), format!("{dest_h} is inside the block being moved")));
        }
        if locked_inside(d, src) {
            return Err(locked_error(&a.anchor));
        }
        if in_locked_control(d, dest) {
            return Err(locked_error(&dest_h));
        }
        let already = if after { next_element(d, dest) == Some(src) } else { next_element(d, src) == Some(dest) };
        if already {
            return Ok(EditOutcome::default());
        }
        if cx.tk.track()
            && d.descendants(src).any(|c| matches!(w_local(d, c), Some("footnoteReference" | "endnoteReference")))
        {
            return Err(OpError::unsupported(
                "direct_only",
                format!("{}: a block with footnotes or endnotes moves only in direct mode", a.anchor),
            ));
        }
        if is_w(d, src, "p") && has_section(d, src) {
            return Err(OpError::unsupported(
                "unsupported_target",
                format!("{}: that paragraph ends a section; it cannot move", a.anchor),
            ));
        }
    }
    let mut tracker = Tracker::new(cx.anchors);
    tracker.touch(cx.doc, part, src);
    let rev0 = cx.tk.revisions.len();
    let d = cx.doc.part_mut(part);
    let old_parent = d.parent(src);
    let mut outside = Vec::new();
    let copy = if cx.tk.track() {
        let copy = d.clone_subtree(src)?;
        scrub_copy(d, copy)?;
        d.insert_before(src, copy)?;
        refresh_ids(d, cx.tk, copy)?;
        Some(copy)
    } else {
        None
    };
    if after {
        d.insert_after(dest, src)?;
    } else {
        d.insert_before(dest, src)?;
    }
    // Mark revisions after the move: where a mark can go depends on the final neighbours.
    if let Some(copy) = copy {
        outside = delete_block_tracked(d, cx.tk, copy)?;
        outside.extend(insert_block_tracked(d, cx.tk, src)?);
    }
    if let Some(par) = old_parent {
        ensure_cell_ends_in_p(d, par)?;
    }
    for q in outside {
        tracker.touch(cx.doc, part, q);
    }
    Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()))
}

// ---------------------------------------------------------------------- merge_paragraphs

#[derive(Deserialize)]
struct MergeArgs {
    first: String,
    second: String,
}

/// `merge_paragraphs {first, second}`: first's paragraph mark goes, second's text joins it.
pub fn merge_paragraphs(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: MergeArgs = args(cx.index, op)?;
    let r1 = resolve(cx.doc, cx.anchors, cx.index, "first", &a.first)?;
    let r2 = resolve(cx.doc, cx.anchors, cx.index, "second", &a.second)?;
    let (Target::Block { para: p1, .. }, Target::Block { para: p2, .. }) = (r1.target, r2.target) else {
        return Err(OpError::invalid(cx.path("first"), "merge_paragraphs joins two paragraphs"));
    };
    if a.first.contains("..") || a.second.contains("..") {
        return Err(OpError::invalid(cx.path("first"), "merge_paragraphs joins two paragraphs, not ranges"));
    }
    let part = p1.part;
    let (first, second) = (p1.node, p2.node);
    let between: Vec<NodeId> = {
        let d = cx.doc.part(part);
        if p2.part != part {
            return Err(OpError::invalid(cx.path("second"), format!("{} must directly follow {}", a.second, a.first)));
        }
        let mut between = Vec::new();
        let mut cur = next_element(d, first);
        while let Some(c) = cur {
            if c == second || is_block(d, c) {
                break;
            }
            between.push(c);
            cur = next_element(d, c);
        }
        if cur != Some(second) {
            return Err(OpError::invalid(cx.path("second"), format!("{} must directly follow {}", a.second, a.first)));
        }
        if in_locked_control(d, first) || in_locked_control(d, second) {
            return Err(locked_error(&a.first));
        }
        if has_section(d, first) {
            return Err(OpError::unsupported(
                "unsupported_target",
                format!("{}: its paragraph mark ends a section; merging would remove the section break", a.first),
            ));
        }
        between
    };
    let mut tracker = Tracker::new(cx.anchors);
    tracker.touch(cx.doc, part, first);
    tracker.touch(cx.doc, part, second);
    let rev0 = cx.tk.revisions.len();
    let d = cx.doc.part_mut(part);
    if cx.tk.track() && !own_mark_ins(d, cx.tk, first) {
        if tw::mark_revision(d, first, "del").is_none() {
            let id = cx.tk.rev();
            tw::mark_paragraph(d, first, "del", id, &cx.tk.stamp)?;
        }
    } else {
        for b in between {
            d.append_child(first, b)?;
        }
        for c in d.children(second).collect::<Vec<_>>() {
            if d.is_element_node(c) && is_w(d, c, "pPr") {
                continue;
            }
            d.append_child(first, c)?;
        }
        let sect = w_child(d, second, "pPr").and_then(|ppr| w_child(d, ppr, "sectPr"));
        if let Some(s) = sect {
            let ppr = tw::ensure_ppr(d, first)?;
            tw::insert_ordered(d, ppr, s, PPR_ORDER)?;
        }
        // The merged paragraph's mark is second's: carry its revision state over.
        if let Some(mr2) = tw::mark_rpr(d, second) {
            let keep: Vec<NodeId> =
                tw::elements(d, mr2).into_iter().filter(|&c| matches!(w_local(d, c), Some("ins" | "del"))).collect();
            if let Some(mr1) = tw::mark_rpr(d, first) {
                for c in tw::elements(d, mr1) {
                    if matches!(w_local(d, c), Some("ins" | "del")) {
                        d.remove(c)?;
                    }
                }
            }
            if !keep.is_empty() {
                let ppr = tw::ensure_ppr(d, first)?;
                let mr1 = tw::ensure_child(d, ppr, "rPr", PPR_ORDER)?;
                for c in keep {
                    tw::insert_ordered(d, mr1, c, RPR_ORDER)?;
                }
            }
        } else if let Some(mr1) = tw::mark_rpr(d, first) {
            for c in tw::elements(d, mr1) {
                if matches!(w_local(d, c), Some("ins" | "del")) {
                    d.remove(c)?;
                }
            }
        }
        d.remove(second)?;
    }
    Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()))
}

// ---------------------------------------------------------------------- set_paragraph

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Indent {
    left: Option<i64>,
    right: Option<i64>,
    first_line: Option<i64>,
    hanging: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Spacing {
    before: Option<i64>,
    after: Option<i64>,
    line: Option<i64>,
    line_rule: Option<String>,
}

#[derive(Deserialize)]
struct SetParagraphArgs {
    anchor: String,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    align: Option<String>,
    #[serde(default)]
    indent: Option<Indent>,
    #[serde(default)]
    spacing: Option<Spacing>,
    #[serde(default)]
    keep_with_next: Option<bool>,
    #[serde(default)]
    page_break_before: Option<bool>,
}

/// Snapshot `p`'s properties once per change set (not for a paragraph this change set inserted).
pub(crate) fn track_ppr(d: &mut Document, tk: &mut Tk, p: NodeId) -> crate::Result<NodeId> {
    let ppr = tw::ensure_ppr(d, p)?;
    if tk.track() && !own_mark_ins(d, tk, p) && w_child(d, ppr, "pPrChange").is_none() {
        let id = tk.rev();
        tw::snapshot_ppr(d, ppr, id, &tk.stamp)?;
    }
    Ok(ppr)
}

fn set_toggle(d: &mut Document, ppr: NodeId, local: &str, on: bool) -> crate::Result<()> {
    if on {
        let el = tw::ensure_child(d, ppr, local, PPR_ORDER)?;
        tw::remove_w(d, el, "val")?;
    } else {
        if let Some(el) = w_child(d, ppr, local) {
            d.remove(el)?;
        }
        if w_child(d, ppr, "pStyle").is_some() {
            let el = tw::ensure_child(d, ppr, local, PPR_ORDER)?;
            tw::set_w(d, el, "val", "0")?;
        }
    }
    Ok(())
}

/// `set_paragraph {anchor, style?, align?, indent?, spacing?, keep_with_next?, page_break_before?}`.
pub fn set_paragraph(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: SetParagraphArgs = args(cx.index, op)?;
    let nothing = a.style.is_none()
        && a.align.is_none()
        && a.indent.is_none()
        && a.spacing.is_none()
        && a.keep_with_next.is_none()
        && a.page_break_before.is_none();
    if nothing {
        return Err(OpError::invalid(cx.path("anchor"), "set_paragraph needs at least one property to set"));
    }
    let jc = match a.align.as_deref() {
        None => None,
        Some("left") => Some("left"),
        Some("center") => Some("center"),
        Some("right") => Some("right"),
        Some("justify") => Some("both"),
        Some(_) => return Err(OpError::invalid(cx.path("align"), "align is left, center, right or justify")),
    };
    if let Some(i) = &a.indent {
        if i.first_line.is_some() && i.hanging.is_some() {
            return Err(OpError::invalid(cx.path("indent"), "give first_line or hanging, not both"));
        }
        if i.hanging.is_some_and(|h| h < 0) {
            return Err(OpError::invalid(cx.path("indent/hanging"), "hanging is not negative"));
        }
    }
    let line_rule = match a.spacing.as_ref().and_then(|s| s.line_rule.as_deref()) {
        None => None,
        Some("auto") => Some("auto"),
        Some("exact") => Some("exact"),
        Some("at_least") => Some("atLeast"),
        Some(_) => return Err(OpError::invalid(cx.path("spacing/line_rule"), "line_rule is auto, exact or at_least")),
    };
    if let Some(s) = &a.spacing
        && [s.before, s.after, s.line].iter().any(|v| v.is_some_and(|v| v < 0))
    {
        return Err(OpError::invalid(cx.path("spacing"), "spacing values are not negative"));
    }
    let r = resolve(cx.doc, cx.anchors, cx.index, "anchor", &a.anchor)?;
    let sheet = cx.doc.style_sheet()?;
    let style = match &a.style {
        Some(s) => resolve_style(sheet.as_ref(), s, cx.path("style"))?,
        None => None,
    };
    {
        let d = cx.doc.part(r.part);
        if r.paras.iter().any(|&p| in_locked_control(d, p)) {
            return Err(locked_error(&a.anchor));
        }
    }
    let mut tracker = Tracker::new(cx.anchors);
    let rev0 = cx.tk.revisions.len();
    let d = cx.doc.part_mut(r.part);
    for &p in &r.paras {
        let ppr = track_ppr(d, cx.tk, p)?;
        if let Some(s) = &style {
            set_style(d, sheet.as_ref(), p, s)?;
        }
        if let Some(v) = jc {
            let el = tw::ensure_child(d, ppr, "jc", PPR_ORDER)?;
            tw::set_w(d, el, "val", v)?;
        }
        if let Some(i) = &a.indent {
            let ind = tw::ensure_child(d, ppr, "ind", PPR_ORDER)?;
            if let Some(v) = i.left {
                tw::set_w(d, ind, "left", &v.to_string())?;
                tw::remove_w(d, ind, "start")?;
                tw::remove_w(d, ind, "leftChars")?;
                tw::remove_w(d, ind, "startChars")?;
            }
            if let Some(v) = i.right {
                tw::set_w(d, ind, "right", &v.to_string())?;
                tw::remove_w(d, ind, "end")?;
                tw::remove_w(d, ind, "rightChars")?;
                tw::remove_w(d, ind, "endChars")?;
            }
            if let Some(v) = i.first_line {
                tw::set_w(d, ind, "firstLine", &v.to_string())?;
                for k in ["hanging", "hangingChars", "firstLineChars"] {
                    tw::remove_w(d, ind, k)?;
                }
            }
            if let Some(v) = i.hanging {
                tw::set_w(d, ind, "hanging", &v.to_string())?;
                for k in ["firstLine", "hangingChars", "firstLineChars"] {
                    tw::remove_w(d, ind, k)?;
                }
            }
        }
        if let Some(s) = &a.spacing {
            let sp = tw::ensure_child(d, ppr, "spacing", PPR_ORDER)?;
            if let Some(v) = s.before {
                tw::set_w(d, sp, "before", &v.to_string())?;
                tw::remove_w(d, sp, "beforeAutospacing")?;
                tw::remove_w(d, sp, "beforeLines")?;
            }
            if let Some(v) = s.after {
                tw::set_w(d, sp, "after", &v.to_string())?;
                tw::remove_w(d, sp, "afterAutospacing")?;
                tw::remove_w(d, sp, "afterLines")?;
            }
            if let Some(v) = s.line {
                tw::set_w(d, sp, "line", &v.to_string())?;
            }
            if let Some(v) = line_rule {
                tw::set_w(d, sp, "lineRule", v)?;
            }
        }
        if let Some(on) = a.keep_with_next {
            set_toggle(d, ppr, "keepNext", on)?;
        }
        if let Some(on) = a.page_break_before {
            set_toggle(d, ppr, "pageBreakBefore", on)?;
        }
    }
    for &p in &r.paras {
        tracker.touch(cx.doc, r.part, p);
    }
    Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()))
}

// ---------------------------------------------------------------------- insert_break

#[derive(Deserialize)]
struct BreakArgs {
    after: String,
    kind: String,
    #[serde(default)]
    section_type: Option<String>,
}

/// `insert_break {after, kind: page|column|section, section_type?}`.
pub fn insert_break(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: BreakArgs = args(cx.index, op)?;
    if !matches!(a.kind.as_str(), "page" | "column" | "section") {
        return Err(OpError::invalid(cx.path("kind"), "kind is page, column or section"));
    }
    if a.section_type.is_some() && a.kind != "section" {
        return Err(OpError::invalid(cx.path("section_type"), "section_type goes with kind section"));
    }
    let sect_type = match a.section_type.as_deref() {
        None | Some("next") => "nextPage",
        Some("continuous") => "continuous",
        Some("even") => "evenPage",
        Some("odd") => "oddPage",
        Some(_) => {
            return Err(OpError::invalid(cx.path("section_type"), "section_type is next, continuous, even or odd"));
        }
    };
    let r = resolve(cx.doc, cx.anchors, cx.index, "after", &a.after)?;
    let anchor = block_node(cx, &r, "after")?;
    let part = r.part;
    if in_locked_control(cx.doc.part(part), anchor) {
        return Err(locked_error(&a.after));
    }
    let mut tracker = Tracker::new(cx.anchors);
    let rev0 = cx.tk.revisions.len();
    if a.kind == "section" {
        let governing = {
            let d = cx.doc.part(part);
            let body_level = d.parent(anchor).is_some_and(|b| is_w(d, b, "body"));
            if !body_level || !is_w(d, anchor, "p") {
                return Err(OpError::unsupported(
                    "unsupported_target",
                    format!("{}: a section break goes after a paragraph of the main text", a.after),
                ));
            }
            if has_section(d, anchor) {
                return Err(OpError::invalid(cx.path("after"), format!("{} already ends a section", a.after)));
            }
            // The sectPr that governs the anchor: the next paragraph-level one, else the body's.
            let mut found = None;
            let mut cur = next_element(d, anchor);
            while let Some(c) = cur {
                if is_w(d, c, "p") && has_section(d, c) {
                    found = w_child(d, c, "pPr").and_then(|ppr| w_child(d, ppr, "sectPr")).map(|s| (s, Some(c)));
                    break;
                }
                if is_w(d, c, "sectPr") {
                    found = Some((c, None));
                    break;
                }
                cur = next_element(d, c);
            }
            found.ok_or_else(|| {
                OpError::unsupported("unsupported_target", "the document has no section properties to copy")
            })?
        };
        let (gov, gov_p) = governing;
        let d = cx.doc.part_mut(part);
        let copy = d.clone_subtree(gov)?;
        if let Some(ch) = w_child(d, copy, "sectPrChange") {
            d.remove(ch)?;
        }
        let p = tw::new_w(d, "p")?;
        d.insert_after(anchor, p)?;
        let ppr = tw::new_w(d, "pPr")?;
        d.append_child(p, ppr)?;
        d.append_child(ppr, copy)?;
        let current =
            w_child(d, gov, "type").and_then(|t| w_attr(d, t, "val")).unwrap_or_else(|| "nextPage".to_owned());
        if current != sect_type {
            if cx.tk.track() && w_child(d, gov, "sectPrChange").is_none() {
                let snap = d.clone_subtree(gov)?;
                for c in tw::elements(d, snap) {
                    if matches!(w_local(d, c), Some("headerReference" | "footerReference" | "sectPrChange")) {
                        d.remove(c)?;
                    }
                }
                for q in d.attrs(snap).map(|(q, _)| q.to_owned()).collect::<Vec<_>>() {
                    d.remove_attr(snap, &q)?;
                }
                let ch = tw::revision_el(d, "sectPrChange", cx.tk.rev(), &cx.tk.stamp)?;
                d.append_child(ch, snap)?;
                tw::insert_ordered(d, gov, ch, SECT_ORDER)?;
            }
            let t = tw::ensure_child(d, gov, "type", SECT_ORDER)?;
            tw::set_w(d, t, "val", sect_type)?;
        }
        if cx.tk.track() {
            let id = cx.tk.rev();
            tw::mark_paragraph(d, p, "ins", id, &cx.tk.stamp)?;
        }
        if let Some(gp) = gov_p {
            tracker.touch(cx.doc, part, gp);
        }
        return Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()));
    }
    let d = cx.doc.part_mut(part);
    let template = if is_w(d, anchor, "p") { Some(anchor) } else { None };
    let p = tw::new_w(d, "p")?;
    d.insert_after(anchor, p)?;
    if let Some(ppr) = template_ppr(d, template)? {
        for c in tw::elements(d, ppr) {
            if matches!(w_local(d, c), Some("numPr" | "pageBreakBefore" | "outlineLvl")) {
                d.remove(c)?;
            }
        }
        d.append_child(p, ppr)?;
    }
    let run = tw::make_run(d, None, "")?;
    let br = tw::new_w(d, "br")?;
    tw::set_w(d, br, "type", &a.kind)?;
    d.append_child(run, br)?;
    if cx.tk.track() {
        let ins = tw::revision_el(d, "ins", cx.tk.rev(), &cx.tk.stamp)?;
        d.append_child(ins, run)?;
        d.append_child(p, ins)?;
        if let Some(q) = mark_inserted(d, cx.tk, p)? {
            tracker.touch(cx.doc, part, q);
        }
    } else {
        d.append_child(p, run)?;
    }
    Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()))
}
