//! The replace core and the text ops built on it: `replace_text`, `find_and_replace`,
//! `split_paragraph`.
//!
//! A replace is a word-level diff of the old text against the new one; each hunk becomes, from
//! right to left, a minimal splice:
//!
//! 1. the new text is built as detached runs whose formatting comes from the old tokens it
//!    replaces (token `j` of the new text takes the run properties of old token
//!    `j * n_old / n_new`), so a superscript `ste` stays superscript;
//! 2. the `w:t` holding each end of the old span is split, and runs are split so the span is a
//!    whole number of runs;
//! 3. the new runs go right after the old span (a pure insertion goes after the run on its left),
//!    climbing out of other authors' insertions and, for a pure insertion at their edge, out of
//!    hyperlinks, smart tags, custom XML, simple fields and content controls;
//! 4. in track mode the new runs are wrapped in one `w:ins` and the old runs in `w:del`; text
//!    inside this transaction's own `w:ins` is simply rewritten. In direct mode the old runs go.
//!
//! Every byte outside the touched runs stays as it was.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;

use super::find;
use super::{Cx, OpError, OpResult, Tk, args, resolve};
use crate::edit::{EditOutcome, Tracker};
use crate::track::diff::{diff, tokenize};
use crate::track::write::{self as tw, is_w, w_child, w_local};
use crate::wml::text::{TextMode, text, visit};
use crate::wml::view::Sdt;
use crate::wml::{FieldKind, Loc, PartIdx, fields};
use crate::xml::{Document, NodeId, ns};

// ---------------------------------------------------------------------- atoms

/// A piece of a paragraph's accepted-mode text: bytes `[start, end)`, produced by `el` (a `w:t`,
/// `w:tab`, `w:br`, …, or an `m:t`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Atom {
    pub el: NodeId,
    pub start: usize,
    pub end: usize,
}

/// The non-empty text atoms of `p`, in order.
pub(crate) fn atoms(d: &Document, p: NodeId) -> Vec<Atom> {
    let mut out = Vec::new();
    let mut at = 0usize;
    visit(d, p, TextMode::AcceptAll, |el, s| {
        if !s.is_empty() {
            out.push(Atom { el, start: at, end: at + s.len() });
        }
        at += s.len();
    });
    out
}

/// The `w:r` an atom element sits in.
pub(crate) fn run_of(d: &Document, el: NodeId) -> Option<NodeId> {
    d.parent(el).filter(|&r| is_w(d, r, "r"))
}

/// The ancestor of `n` (or `n`) that is a direct child of `p`.
pub(crate) fn top_child(d: &Document, p: NodeId, n: NodeId) -> NodeId {
    let mut cur = n;
    while let Some(par) = d.parent(cur) {
        if par == p {
            break;
        }
        cur = par;
    }
    cur
}

/// Accepted-mode text of a paragraph.
pub(crate) fn para_text(d: &Document, p: NodeId) -> String {
    text(d, p, TextMode::AcceptAll)
}

// ---------------------------------------------------------------------- locks

/// Runs whose text must not be edited in place: results of fields Word recomputes (cross
/// references, page numbers, dates, TOC, …). Hyperlink, form-text and unknown fields are free.
pub(crate) struct Locks {
    field_runs: HashSet<NodeId>,
}

impl Locks {
    pub(crate) fn of(d: &Document) -> Locks {
        let mut field_runs = HashSet::new();
        for f in fields(d, d.root()) {
            if matches!(f.kind, FieldKind::Hyperlink | FieldKind::FormText | FieldKind::Other(_) | FieldKind::Empty) {
                continue;
            }
            field_runs.extend(f.result_runs.iter().map(|r| r.0));
        }
        Locks { field_runs }
    }

    fn in_field(&self, run: NodeId) -> bool {
        self.field_runs.contains(&run)
    }
}

/// Is `n` inside a content control whose content is locked?
pub(crate) fn in_locked_control(d: &Document, n: NodeId) -> bool {
    d.ancestors(n).any(|a| is_w(d, a, "sdt") && Sdt(a).content_locked(d))
}

pub(crate) fn preserve_only(handle: &str) -> OpError {
    OpError::unsupported(
        "preserve_only",
        format!("{handle}: the text there is an equation or other content this engine keeps but does not edit"),
    )
    .with_anchor(handle)
}

fn field_error(handle: &str) -> OpError {
    OpError::in_field(
        handle,
        "field_result",
        format!(
            "{handle}: that text is the result of a field Word recomputes (cross-reference, date, page number, …); edit around it or use fill"
        ),
    )
}

pub(crate) fn locked_error(handle: &str) -> OpError {
    OpError {
        reason: Some("locked_control".into()),
        ..OpError::new("PROTECTED", format!("{handle}: that text is in a locked content control")).with_anchor(handle)
    }
}

/// Check that bytes `[hs, he)` of `p` (or the insertion point `hs == he`) can be edited.
fn check_span(d: &Document, locks: &Locks, p: NodeId, at: &[Atom], hs: usize, he: usize, handle: &str) -> OpResult<()> {
    if in_locked_control(d, p) {
        return Err(locked_error(handle));
    }
    let check_run = |el: NodeId| -> OpResult<()> {
        let Some(r) = run_of(d, el) else { return Err(preserve_only(handle)) };
        if locks.in_field(r) {
            return Err(field_error(handle));
        }
        if in_locked_control(d, r) {
            return Err(locked_error(handle));
        }
        Ok(())
    };
    if hs < he {
        for a in at.iter().filter(|a| a.start < he && a.end > hs) {
            check_run(a.el)?;
            let partial = a.start < hs || a.end > he;
            if partial && !is_w(d, a.el, "t") {
                return Err(preserve_only(handle));
            }
        }
        return Ok(());
    }
    // A pure insertion.
    if let Some(a) = at.iter().find(|a| a.start < hs && hs < a.end) {
        if !is_w(d, a.el, "t") {
            return Err(preserve_only(handle));
        }
        return check_run(a.el);
    }
    let left = at.iter().rev().find(|a| a.end == hs).and_then(|a| run_of(d, a.el));
    let right = at.iter().find(|a| a.start == hs).and_then(|a| run_of(d, a.el));
    if let (Some(l), Some(r)) = (left, right) {
        if locks.in_field(l) && locks.in_field(r) {
            return Err(field_error(handle));
        }
        let lc = d.ancestors(l).find(|&a| is_w(d, a, "sdt") && Sdt(a).content_locked(d));
        let rc = d.ancestors(r).find(|&a| is_w(d, a, "sdt") && Sdt(a).content_locked(d));
        if lc.is_some() && lc == rc {
            return Err(locked_error(handle));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------- tree surgery

/// Property children kept when a container is split in two.
const PROPS: &[&str] = &["rPr", "pPr", "smartTagPr", "customXmlPr", "sdtPr", "sdtEndPr"];

/// Elements whose `w:id` is a revision / annotation id (refreshed on copies).
const REVISION_ELEMENTS: &[&str] = &[
    "ins",
    "del",
    "moveFrom",
    "moveTo",
    "rPrChange",
    "pPrChange",
    "sectPrChange",
    "tblPrChange",
    "trPrChange",
    "tcPrChange",
    "numberingChange",
    "cellIns",
    "cellDel",
    "cellMerge",
];

/// Give every revision element at or below `n` (a fresh copy) a new id: one of ours when the
/// original was, another author's otherwise.
///
/// `n` must be attached to the document: `w:id` is looked up by namespace, which a detached
/// copy can't resolve (the copy would silently keep the original's id).
pub(crate) fn refresh_ids(d: &mut Document, tk: &mut Tk, n: NodeId) -> crate::Result<()> {
    debug_assert!(d.parent(n).is_some(), "refresh_ids on a detached node");
    let nodes: Vec<NodeId> = std::iter::once(n).chain(d.descendants(n)).collect();
    for c in nodes {
        if !d.is_element_node(c) || !w_local(d, c).is_some_and(|l| REVISION_ELEMENTS.contains(&l)) {
            continue;
        }
        let Some(old) = tw::w_attr(d, c, "id") else { continue };
        let id = if tk.is_own(d, c) {
            tk.rev()
        } else {
            let id = tk.foreign();
            if let Ok(o) = old.trim().parse::<u32>() {
                tk.aliases.push((o, id));
            }
            id
        };
        tw::set_w(d, c, "id", &id.to_string())?;
    }
    Ok(())
}

/// Split `parent` before its child `child`: a copy of `parent` (properties kept, ids refreshed)
/// takes `child` and every later sibling and is inserted right after `parent`. Returns the copy.
pub(crate) fn split_before(d: &mut Document, tk: &mut Tk, parent: NodeId, child: NodeId) -> crate::Result<NodeId> {
    let copy = d.clone_subtree(parent)?;
    let kids: Vec<NodeId> = d.children(copy).collect();
    for c in kids {
        let keep = d.is_element_node(c) && w_local(d, c).is_some_and(|l| PROPS.contains(&l));
        if !keep {
            d.remove(c)?;
        }
    }
    // Attach first (ids resolve by namespace), refresh while the copy holds only properties.
    d.insert_after(parent, copy)?;
    refresh_ids(d, tk, copy)?;
    let mut cur = Some(child);
    while let Some(c) = cur {
        cur = d.next_sibling(c);
        d.append_child(copy, c)?;
    }
    Ok(copy)
}

/// Is there content (not properties, not whitespace) before `n` in its parent?
pub(crate) fn content_before(d: &Document, n: NodeId) -> bool {
    let mut cur = d.prev_sibling(n);
    while let Some(c) = cur {
        if d.is_element_node(c) && !w_local(d, c).is_some_and(|l| PROPS.contains(&l)) {
            return true;
        }
        cur = d.prev_sibling(c);
    }
    false
}

/// The next element sibling of `n`.
pub(crate) fn next_element(d: &Document, n: NodeId) -> Option<NodeId> {
    let mut cur = d.next_sibling(n);
    while let Some(c) = cur {
        if d.is_element_node(c) {
            return Some(c);
        }
        cur = d.next_sibling(c);
    }
    None
}

fn needs_preserve(s: &str) -> bool {
    s.starts_with(char::is_whitespace) || s.ends_with(char::is_whitespace) || s.contains("  ")
}

/// Split the `w:t` `t` at byte `k` of its text; returns the new right half (inserted after `t`).
fn split_t(d: &mut Document, t: NodeId, k: usize) -> crate::Result<NodeId> {
    let s = d.text_content(t);
    let (head, tail) = s.split_at(k);
    let t2 = d.clone_subtree(t)?;
    d.set_text(t, head)?;
    d.set_text(t2, tail)?;
    for (n, part) in [(t, head), (t2, tail)] {
        if needs_preserve(part) && d.attr(n, "xml:space").is_none() {
            d.set_attr(n, "xml:space", "preserve")?;
        }
    }
    d.insert_after(t, t2)?;
    Ok(t2)
}

/// Make byte offset `x` of `p`'s text an atom boundary.
pub(crate) fn split_at_offset(d: &mut Document, p: NodeId, x: usize, handle: &str) -> OpResult<()> {
    let at = atoms(d, p);
    if let Some(a) = at.iter().find(|a| a.start < x && x < a.end) {
        if !is_w(d, a.el, "t") {
            return Err(preserve_only(handle));
        }
        split_t(d, a.el, x - a.start)?;
    }
    Ok(())
}

/// Split runs so that the atoms inside `[hs, he)` fill whole runs. Returns those runs in order.
pub(crate) fn isolate(
    d: &mut Document,
    tk: &mut Tk,
    p: NodeId,
    hs: usize,
    he: usize,
    handle: &str,
) -> OpResult<Vec<NodeId>> {
    split_at_offset(d, p, he, handle)?;
    split_at_offset(d, p, hs, handle)?;
    let at = atoms(d, p);
    // (run, first atom element, last atom element)
    let mut groups: Vec<(NodeId, NodeId, NodeId)> = Vec::new();
    for a in at.iter().filter(|a| a.start >= hs && a.end <= he) {
        let r = run_of(d, a.el).ok_or_else(|| preserve_only(handle))?;
        match groups.last_mut() {
            Some(g) if g.0 == r => g.2 = a.el,
            _ => groups.push((r, a.el, a.el)),
        }
    }
    let mut out = Vec::with_capacity(groups.len());
    for (r, first, last) in groups {
        let mut run = r;
        if content_before(d, first) {
            run = split_before(d, tk, run, first)?;
        }
        if let Some(nx) = next_element(d, last) {
            split_before(d, tk, run, nx)?;
        }
        out.push(run);
    }
    Ok(out)
}

/// Where new runs go.
#[derive(Debug, Clone, Copy)]
enum Pos {
    After(NodeId),
    Before(NodeId),
    /// At the end of the paragraph.
    End(NodeId),
}

/// Move an insertion point next to `run` out of wrappers it must not land in. Returns the point
/// and whether it is inside one of this transaction's own insertions (then no new `w:ins`), or,
/// with `mine`, inside a pending insertion by the same author (rewritten in place).
fn place(d: &mut Document, tk: &mut Tk, p: NodeId, start: Pos, pure: bool, mine: bool) -> crate::Result<(Pos, bool)> {
    let mut pos = start;
    // A pure insertion right after a field result goes after the field's end.
    if pure
        && let Pos::After(n) = pos
        && let Some(nx) = next_element(d, n)
    {
        let ends_field = is_w(d, nx, "r")
            && d.child_elements(nx)
                .any(|c| is_w(d, c, "fldChar") && tw::w_attr(d, c, "fldCharType").as_deref() == Some("end"));
        if ends_field {
            pos = Pos::After(nx);
        }
    }
    loop {
        let at = match pos {
            Pos::After(n) | Pos::Before(n) => n,
            Pos::End(_) => return Ok((pos, false)),
        };
        let Some(par) = d.parent(at) else { return Ok((pos, false)) };
        if par == p {
            return Ok((pos, false));
        }
        let at_edge = match pos {
            Pos::After(n) => next_element(d, n).is_none(),
            _ => !content_before(d, at),
        };
        match w_local(d, par).unwrap_or("") {
            "ins" | "moveTo" => {
                if tk.track() && is_w(d, par, "ins") && (tk.is_own(d, par) || (mine && tk.is_mine(d, par))) {
                    tk.absorb(d, par);
                    return Ok((pos, true));
                }
                if !at_edge {
                    match pos {
                        Pos::After(n) => {
                            if let Some(nx) = next_element(d, n) {
                                split_before(d, tk, par, nx)?;
                            }
                        }
                        _ => {
                            split_before(d, tk, par, at)?;
                            pos = Pos::After(par);
                            continue;
                        }
                    }
                }
                pos = match pos {
                    Pos::Before(_) => Pos::Before(par),
                    _ => Pos::After(par),
                };
            }
            "hyperlink" | "smartTag" | "customXml" | "fldSimple" | "sdtContent" | "sdt" if pure && at_edge => {
                pos = match pos {
                    Pos::Before(_) => Pos::Before(par),
                    _ => Pos::After(par),
                };
            }
            _ => return Ok((pos, false)),
        }
    }
}

fn put(d: &mut Document, pos: Pos, nodes: &[NodeId]) -> crate::Result<()> {
    match pos {
        Pos::After(at) => {
            let mut a = at;
            for &n in nodes {
                d.insert_after(a, n)?;
                a = n;
            }
        }
        Pos::Before(at) => {
            for &n in nodes {
                d.insert_before(at, n)?;
            }
        }
        Pos::End(p) => {
            for &n in nodes {
                d.append_child(p, n)?;
            }
        }
    }
    Ok(())
}

/// Insert detached runs at `pos` (wrapped in one tracked `w:ins` in track mode).
fn insert_runs(
    d: &mut Document,
    tk: &mut Tk,
    p: NodeId,
    pos: Pos,
    runs: Vec<NodeId>,
    pure: bool,
    mine: bool,
) -> crate::Result<()> {
    if runs.is_empty() {
        return Ok(());
    }
    let (pos, in_own) = place(d, tk, p, pos, pure, mine)?;
    if tk.track() && !in_own {
        let ins = tw::revision_el(d, "ins", tk.rev(), &tk.stamp)?;
        for r in runs {
            d.append_child(ins, r)?;
        }
        put(d, pos, &[ins])
    } else {
        put(d, pos, &runs)
    }
}

/// Is `n` an element with no element children left?
fn emptied(d: &Document, n: NodeId) -> bool {
    d.child_elements(n).next().is_none()
}

/// Remove `r`, then any wrapper (insertion, hyperlink, smart tag) it leaves empty.
pub(crate) fn remove_run(d: &mut Document, p: NodeId, r: NodeId) -> crate::Result<()> {
    let mut par = d.parent(r);
    d.remove(r)?;
    while let Some(w) = par {
        if w == p || !emptied(d, w) || !matches!(w_local(d, w), Some("ins" | "moveTo" | "hyperlink" | "smartTag")) {
            break;
        }
        par = d.parent(w);
        d.remove(w)?;
    }
    Ok(())
}

/// The pending insertion by this author (see [`Tk::is_mine`]) that run `r` of `p` sits in.
fn mine_ins(d: &Document, tk: &Tk, p: NodeId, r: NodeId) -> Option<NodeId> {
    d.ancestors(r).take_while(|&a| a != p).find(|&a| tk.is_mine(d, a))
}

/// Delete whole runs: tracked (`w:del`, one per sibling group) or removed. Text inside this
/// transaction's own insertions is removed either way; with `mine`, so is text inside pending
/// insertions by the same author.
pub(crate) fn delete_runs(d: &mut Document, tk: &mut Tk, p: NodeId, runs: &[NodeId], mine: bool) -> crate::Result<()> {
    let mut groups: Vec<Vec<NodeId>> = Vec::new();
    for &r in runs {
        let own = tk.track()
            && match d
                .ancestors(r)
                .take_while(|&a| a != p)
                .find(|&a| is_w(d, a, "ins") && (tk.is_own(d, a) || (mine && tk.is_mine(d, a))))
            {
                Some(ins) => {
                    tk.absorb(d, ins);
                    true
                }
                None => false,
            };
        if own || !tk.track() {
            remove_run(d, p, r)?;
            continue;
        }
        match groups.last_mut() {
            Some(g) if g.last().and_then(|&l| next_element(d, l)) == Some(r) => g.push(r),
            _ => groups.push(vec![r]),
        }
    }
    for g in groups {
        let id = tk.rev();
        tw::delete_runs(d, &g, id, &tk.stamp)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------- formatting of new text

/// Run properties for new text: the `w:rPr` of the run the old text at `off` sits in.
fn rpr_at(d: &Document, at: &[Atom], off: usize) -> Option<NodeId> {
    let a = at.iter().find(|a| a.start <= off && off < a.end)?;
    run_of(d, a.el).and_then(|r| w_child(d, r, "rPr"))
}

fn has_vert_align(d: &Document, rpr: Option<NodeId>) -> bool {
    rpr.is_some_and(|r| w_child(d, r, "vertAlign").is_some())
}

/// Build the detached runs for `new`, replacing `[hs, he)` of `p` (text `ptext`).
fn build_runs(
    d: &mut Document,
    p: NodeId,
    at: &[Atom],
    ptext: &str,
    hs: usize,
    he: usize,
    new: &str,
) -> crate::Result<Vec<NodeId>> {
    let ntoks = tokenize(new);
    let mut spans: Vec<(Option<NodeId>, std::ops::Range<usize>)> = Vec::new();
    if hs == he {
        // Pure insertion: the formatting on the left, unless that is a superscript / subscript.
        let left = at.iter().rev().find(|a| a.end == hs).or_else(|| at.iter().find(|a| a.start < hs && hs < a.end));
        let right = at.iter().find(|a| a.start == hs);
        let lr = left.and_then(|a| run_of(d, a.el)).and_then(|r| w_child(d, r, "rPr"));
        let rr = right.and_then(|a| run_of(d, a.el)).and_then(|r| w_child(d, r, "rPr"));
        let rpr = match (left, right) {
            (Some(_), Some(_)) if has_vert_align(d, lr) && !has_vert_align(d, rr) => rr,
            (Some(_), _) => lr,
            (None, Some(_)) => rr,
            (None, None) => tw::mark_rpr(d, p),
        };
        spans.push((rpr, 0..new.len()));
    } else {
        let otoks = tokenize(&ptext[hs..he]);
        for (j, nt) in ntoks.iter().enumerate() {
            let ws = new[nt.clone()].chars().all(char::is_whitespace);
            let rpr = match spans.last() {
                Some(last) if ws => last.0,
                _ => {
                    let i = if ntoks.len() <= 1 {
                        0
                    } else {
                        (j * otoks.len() / ntoks.len()).min(otoks.len().saturating_sub(1))
                    };
                    let off = hs + otoks.get(i).map_or(0, |r| r.start);
                    rpr_at(d, at, off).or_else(|| rpr_at(d, at, hs))
                }
            };
            match spans.last_mut() {
                Some(last) if last.0 == rpr => last.1.end = nt.end,
                _ => spans.push((rpr, nt.clone())),
            }
        }
    }
    let mut runs = Vec::with_capacity(spans.len());
    for (rpr, range) in spans {
        runs.push(tw::make_run(d, rpr, &new[range])?);
    }
    Ok(runs)
}

// ---------------------------------------------------------------------- the core

/// Replace the byte range `span` of `p`'s accepted text with `new`, as word-level hunks.
/// Returns whether anything changed.
pub(crate) fn replace_range(
    d: &mut Document,
    tk: &mut Tk,
    locks: &Locks,
    p: NodeId,
    span: std::ops::Range<usize>,
    new: &str,
    handle: &str,
) -> OpResult<bool> {
    let (start, end) = (span.start, span.end);
    let ptext = para_text(d, p);
    let old = &ptext[start..end];
    if old == new {
        return Ok(false);
    }
    let hunks = diff(old, new);
    let at = atoms(d, p);
    for h in &hunks {
        check_span(d, locks, p, &at, start + h.old.start, start + h.old.end, handle)?;
    }
    for h in hunks.iter().rev() {
        splice(d, tk, p, start + h.old.start, start + h.old.end, &new[h.new.clone()], handle)?;
    }
    Ok(true)
}

/// One hunk: bytes `[hs, he)` of `p` become `new`.
fn splice(d: &mut Document, tk: &mut Tk, p: NodeId, hs: usize, he: usize, new: &str, handle: &str) -> OpResult<()> {
    let ptext = para_text(d, p);
    let at = atoms(d, p);
    let runs = if new.is_empty() { Vec::new() } else { build_runs(d, p, &at, &ptext, hs, he, new)? };
    let pure = hs == he;
    let old_runs = if pure {
        split_at_offset(d, p, hs, handle)?;
        // Cut the run there as well, or the new runs land after the whole run instead of at `hs`.
        let at = atoms(d, p);
        if let Some(l) = at.iter().rev().find(|a| a.end == hs)
            && let (Some(r), Some(nx)) = (run_of(d, l.el), next_element(d, l.el))
        {
            split_before(d, tk, r, nx)?;
        }
        Vec::new()
    } else {
        isolate(d, tk, p, hs, he, handle)?
    };
    // A hunk wholly inside pending insertions by this author rewrites them in place; one that
    // reaches outside them is tracked like any other (so rejecting it restores the old text).
    let mine = old_runs.iter().all(|&r| mine_ins(d, tk, p, r).is_some());
    if !runs.is_empty() {
        let pos = match old_runs.last() {
            Some(&last) => Pos::After(last),
            None => {
                let at = atoms(d, p);
                let node = |a: &Atom| run_of(d, a.el).unwrap_or_else(|| top_child(d, p, a.el));
                match (at.iter().rev().find(|a| a.end == hs), at.iter().find(|a| a.start == hs)) {
                    (Some(l), _) => Pos::After(node(l)),
                    (None, Some(r)) => Pos::Before(node(r)),
                    (None, None) => Pos::End(p),
                }
            }
        };
        insert_runs(d, tk, p, pos, runs, pure, mine)?;
    }
    delete_runs(d, tk, p, &old_runs, mine)?;
    Ok(())
}

// ---------------------------------------------------------------------- ops

pub(crate) fn handle_for(cx: &Cx, part: PartIdx, p: NodeId) -> String {
    cx.anchors.handle_of(Loc::new(part, p)).unwrap_or("?").to_owned()
}

#[derive(Deserialize)]
struct ReplaceArgs {
    anchor: String,
    replace: String,
    #[serde(default)]
    find: Option<String>,
    #[serde(default)]
    occurrence: Option<u32>,
}

/// `replace_text {anchor, find?, occurrence?, replace}`.
pub fn replace_text(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: ReplaceArgs = args(cx.index, op)?;
    let r = resolve(cx.doc, cx.anchors, cx.index, "anchor", &a.anchor)?;
    let mut tracker = Tracker::new(cx.anchors);
    let (p, start, end) = {
        let d = cx.doc.part(r.part);
        match &a.find {
            Some(f) => {
                let texts = find::texts(d, &r.paras);
                let hits = find::find_all(&texts, f);
                let path = cx.path("occurrence");
                let h = find::pick(&hits, &texts, f, a.occurrence, &a.anchor, path, &|i| {
                    handle_for(cx, r.part, r.paras[i])
                })?;
                (r.paras[h.para], h.start, h.end)
            }
            None => {
                let Some(p) = r.single_para() else {
                    return Err(OpError::invalid(
                        cx.path("anchor"),
                        format!("{} covers several paragraphs; give find to pick the text to replace", a.anchor),
                    ));
                };
                (p, 0, para_text(d, p).len())
            }
        }
    };
    let locks = Locks::of(cx.doc.part(r.part));
    let rev0 = cx.tk.revisions.len();
    let d = cx.doc.part_mut(r.part);
    replace_range(d, cx.tk, &locks, p, start..end, &a.replace, &a.anchor)?;
    tracker.touch(cx.doc, r.part, p);
    Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()))
}

#[derive(Deserialize)]
struct FindReplaceArgs {
    find: String,
    replace: String,
    count: u32,
    #[serde(default)]
    anchor: Option<String>,
}

/// Body paragraphs (cells included, text boxes excluded) of the main part.
pub(crate) fn body_paras(doc: &crate::wml::WordDoc) -> (PartIdx, Vec<NodeId>) {
    let part = doc.main_part();
    let d = doc.part(part);
    let body = d.find_child(d.root(), ns::W, "body").unwrap_or(d.root());
    (part, super::paras_in(d, body))
}

/// `find_and_replace {find, replace, count, anchor?}`: every occurrence, guarded by `count`.
pub fn find_and_replace(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: FindReplaceArgs = args(cx.index, op)?;
    if a.find.is_empty() {
        return Err(OpError::invalid(cx.path("find"), "find is empty"));
    }
    let (part, paras, label) = match &a.anchor {
        Some(h) => {
            let r = resolve(cx.doc, cx.anchors, cx.index, "anchor", h)?;
            (r.part, r.paras, h.clone())
        }
        None => {
            let (part, paras) = body_paras(cx.doc);
            (part, paras, "the body".to_owned())
        }
    };
    let texts = find::texts(cx.doc.part(part), &paras);
    let hits = find::find_all(&texts, &a.find);
    if hits.len() != a.count as usize {
        let where_: Vec<String> = hits.iter().take(20).map(|h| handle_for(cx, part, paras[h.para])).collect();
        let mut msg = format!("{:?} occurs {} time(s) in {label}, count says {}", a.find, hits.len(), a.count);
        if !where_.is_empty() {
            msg.push_str(&format!(" (in {})", where_.join(", ")));
        }
        return Err(OpError::invalid(cx.path("count"), msg));
    }
    let mut tracker = Tracker::new(cx.anchors);
    let locks = Locks::of(cx.doc.part(part));
    let handles: Vec<String> = hits.iter().map(|h| handle_for(cx, part, paras[h.para])).collect();
    let rev0 = cx.tk.revisions.len();
    let d = cx.doc.part_mut(part);
    for (h, handle) in hits.iter().zip(&handles).rev() {
        replace_range(d, cx.tk, &locks, paras[h.para], h.start..h.end, &a.replace, handle)?;
    }
    for h in &hits {
        tracker.touch(cx.doc, part, paras[h.para]);
    }
    Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()))
}

#[derive(Deserialize)]
struct SplitArgs {
    anchor: String,
    at_text: String,
    #[serde(default)]
    occurrence: Option<u32>,
}

/// Containers a paragraph split may cut through.
const SPLITTABLE: &[&str] = &["r", "ins", "del", "moveFrom", "moveTo", "hyperlink", "smartTag", "customXml"];

/// `split_paragraph {anchor, at_text, occurrence?}`: `at_text` starts the new second paragraph.
pub fn split_paragraph(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: SplitArgs = args(cx.index, op)?;
    let r = resolve(cx.doc, cx.anchors, cx.index, "anchor", &a.anchor)?;
    let Some(p) = r.single_para() else {
        return Err(OpError::invalid(cx.path("anchor"), "split_paragraph takes one paragraph"));
    };
    let x = {
        let d = cx.doc.part(r.part);
        let texts = find::texts(d, &[p]);
        let hits = find::find_all(&texts, &a.at_text);
        let path = cx.path("occurrence");
        let h = find::pick(&hits, &texts, &a.at_text, a.occurrence, &a.anchor, path, &|_| a.anchor.clone())?;
        if h.start == 0 {
            return Err(OpError::invalid(cx.path("at_text"), "at_text starts the paragraph: nothing to split off"));
        }
        if in_locked_control(d, p) {
            return Err(locked_error(&a.anchor));
        }
        h.start
    };
    let mut tracker = Tracker::new(cx.anchors);
    let rev0 = cx.tk.revisions.len();
    let handle = a.anchor.clone();
    let d = cx.doc.part_mut(r.part);
    split_at_offset(d, p, x, &handle)?;
    let at = atoms(d, p);
    let first = at.iter().find(|a| a.start >= x).map(|a| a.el).ok_or_else(|| preserve_only(&handle))?;
    // Climb to a direct child of p, splitting containers on the way.
    let mut node = first;
    loop {
        let Some(par) = d.parent(node) else { return Err(preserve_only(&handle)) };
        if par == p {
            break;
        }
        if content_before(d, node) {
            if !w_local(d, par).is_some_and(|l| SPLITTABLE.contains(&l)) || d.namespace(par) != Some(ns::W) {
                return Err(preserve_only(&handle));
            }
            node = split_before(d, cx.tk, par, node)?;
        } else {
            node = par;
        }
    }
    // The new paragraph: same properties, the original mark (and section break).
    let p2 = d.clone_subtree(p)?;
    for c in d.children(p2).collect::<Vec<_>>() {
        if !(d.is_element_node(c) && is_w(d, c, "pPr")) {
            d.remove(c)?;
        }
    }
    tw::strip_para_ids(d, p2)?;
    if let Some(ppr2) = w_child(d, p2, "pPr")
        && let Some(ch) = w_child(d, ppr2, "pPrChange")
    {
        d.remove(ch)?;
    }
    if let Some(ppr) = w_child(d, p, "pPr") {
        if let Some(s) = w_child(d, ppr, "sectPr") {
            d.remove(s)?;
        }
        if let Some(mr) = w_child(d, ppr, "rPr") {
            for c in d.child_elements(mr).collect::<Vec<_>>() {
                if matches!(w_local(d, c), Some("ins" | "del" | "moveFrom" | "moveTo")) {
                    d.remove(c)?;
                }
            }
        }
    }
    let mut cur = Some(node);
    while let Some(c) = cur {
        cur = d.next_sibling(c);
        d.append_child(p2, c)?;
    }
    d.insert_after(p, p2)?;
    if cx.tk.track() {
        let id = cx.tk.rev();
        tw::mark_paragraph(d, p, "ins", id, &cx.tk.stamp)?;
    }
    tracker.touch(cx.doc, r.part, p);
    Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()))
}
