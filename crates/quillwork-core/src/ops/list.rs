//! List ops: `set_list`, `restart_numbering`, `change_level`.
//!
//! Lists are `w:numPr` on the paragraph (tracked through `w:pPrChange`) plus definitions in
//! `numbering.xml`. A shared `w:abstractNum` is never edited: a new list reuses a definition with
//! the same look, or a new `w:num` / `w:abstractNum` is appended.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

use super::para::{ListArg, NUMPR_ORDER, apply_list, track_ppr};
use super::text::{in_locked_control, locked_error};
use super::{Cx, OpError, OpResult, args, paras_in, resolve};
use crate::edit::{EditOutcome, Tracker};
use crate::track::write::{self as tw, PPR_ORDER, is_w, w_attr, w_child};
use crate::wml::numbering::Numbering;
use crate::wml::view::Paragraph;
use crate::wml::{PartIdx, PartKind, WordDoc};
use crate::xml::{Document, NodeId, ns};

const NUMBERING_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
const NUMBERING_CT: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bullet,
    Numbered,
}

impl Kind {
    fn matches(self, fmt: &str) -> bool {
        match self {
            Kind::Bullet => fmt == "bullet",
            Kind::Numbered => fmt == "decimal",
        }
    }
}

/// What `numbering.xml` holds, as far as choosing a definition goes.
struct Defs {
    /// abstractNumId → level-0 numFmt (definitions linked to a numbering style are skipped).
    abstract_fmt: HashMap<i64, String>,
    /// numId → (abstractNumId, has overrides).
    nums: HashMap<i64, (i64, bool)>,
    max_abstract: i64,
    max_num: i64,
}

fn int_attr(d: &Document, n: NodeId, local: &str) -> Option<i64> {
    w_attr(d, n, local).and_then(|v| v.trim().parse().ok())
}

fn child_int(d: &Document, n: NodeId, local: &str) -> Option<i64> {
    w_child(d, n, local).and_then(|c| int_attr(d, c, "val"))
}

fn defs(d: &Document) -> Defs {
    let mut out = Defs { abstract_fmt: HashMap::new(), nums: HashMap::new(), max_abstract: -1, max_num: 0 };
    for n in d.child_elements(d.root()) {
        if is_w(d, n, "abstractNum") {
            let Some(id) = int_attr(d, n, "abstractNumId") else { continue };
            out.max_abstract = out.max_abstract.max(id);
            if w_child(d, n, "numStyleLink").is_some() {
                continue;
            }
            let lvl0 = d.child_elements(n).find(|&l| is_w(d, l, "lvl") && int_attr(d, l, "ilvl") == Some(0));
            let fmt = lvl0
                .and_then(|l| w_child(d, l, "numFmt"))
                .and_then(|f| w_attr(d, f, "val"))
                .unwrap_or_else(|| "decimal".to_owned());
            out.abstract_fmt.insert(id, fmt);
        } else if is_w(d, n, "num") {
            let Some(id) = int_attr(d, n, "numId") else { continue };
            out.max_num = out.max_num.max(id);
            let Some(aid) = child_int(d, n, "abstractNumId") else { continue };
            let overrides = w_child(d, n, "lvlOverride").is_some();
            out.nums.insert(id, (aid, overrides));
        }
    }
    out
}

/// The numbering part, created empty when the document has none.
fn ensure_numbering(doc: &mut WordDoc) -> OpResult<PartIdx> {
    if let Some(p) = doc.numbering_part() {
        return Ok(p);
    }
    let mut name = "word/numbering.xml".to_owned();
    let mut i = 1;
    while doc.part_idx(&name).is_some() || doc.package().is_some_and(|p| p.contains(&name)) {
        name = format!("word/numbering{i}.xml");
        i += 1;
    }
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<w:numbering xmlns:w=\"{}\"></w:numbering>",
        ns::W
    );
    Ok(doc.add_part(&name, PartKind::Numbering, NUMBERING_REL, NUMBERING_CT, xml.into_bytes())?)
}

fn child(d: &mut Document, parent: NodeId, local: &str, attrs: &[(&str, &str)]) -> crate::Result<NodeId> {
    let el = tw::new_w(d, local)?;
    for (k, v) in attrs {
        tw::set_w(d, el, k, v)?;
    }
    d.append_child(parent, el)?;
    Ok(el)
}

/// Insert an `abstractNum` before the first `num` (schema order), a `num` after the last one.
fn place(d: &mut Document, el: NodeId, abstract_: bool) -> crate::Result<()> {
    let root = d.root();
    let kids: Vec<NodeId> = d.child_elements(root).collect();
    if abstract_ {
        match kids.iter().find(|&&c| is_w(d, c, "num") || is_w(d, c, "numIdMacAtCleanup")) {
            Some(&f) => d.insert_before(f, el),
            None => d.append_child(root, el),
        }
    } else {
        match kids.iter().find(|&&c| is_w(d, c, "numIdMacAtCleanup")) {
            Some(&f) => d.insert_before(f, el),
            None => d.append_child(root, el),
        }
    }
}

/// A nine-level definition with Word's default look.
fn mint_abstract(d: &mut Document, id: i64, kind: Kind) -> crate::Result<()> {
    let a = tw::new_w(d, "abstractNum")?;
    tw::set_w(d, a, "abstractNumId", &id.to_string())?;
    child(d, a, "multiLevelType", &[("val", "hybridMultilevel")])?;
    for i in 0..9usize {
        let lvl = child(d, a, "lvl", &[("ilvl", &i.to_string())])?;
        child(d, lvl, "start", &[("val", "1")])?;
        let (fmt, text) = match kind {
            Kind::Bullet => ("bullet", ["\u{2022}", "o", "\u{25AA}"][i % 3].to_owned()),
            Kind::Numbered => (["decimal", "lowerLetter", "lowerRoman"][i % 3], format!("%{}.", i + 1)),
        };
        child(d, lvl, "numFmt", &[("val", fmt)])?;
        child(d, lvl, "lvlText", &[("val", &text)])?;
        let jc = if kind == Kind::Numbered && i % 3 == 2 { "right" } else { "left" };
        child(d, lvl, "lvlJc", &[("val", jc)])?;
        let ppr = child(d, lvl, "pPr", &[])?;
        let left = (720 * (i + 1)).to_string();
        let hanging = if kind == Kind::Numbered && i % 3 == 2 { "180" } else { "360" };
        child(d, ppr, "ind", &[("left", &left), ("hanging", hanging)])?;
        if kind == Kind::Bullet && i % 3 == 1 {
            let rpr = child(d, lvl, "rPr", &[])?;
            child(d, rpr, "rFonts", &[("ascii", "Courier New"), ("hAnsi", "Courier New"), ("cs", "Courier New")])?;
        }
    }
    place(d, a, true)
}

fn mint_num(d: &mut Document, id: i64, abstract_id: i64, start: Option<(usize, i64)>) -> crate::Result<()> {
    let n = tw::new_w(d, "num")?;
    tw::set_w(d, n, "numId", &id.to_string())?;
    child(d, n, "abstractNumId", &[("val", &abstract_id.to_string())])?;
    if let Some((ilvl, s)) = start {
        let ov = child(d, n, "lvlOverride", &[("ilvl", &ilvl.to_string())])?;
        child(d, ov, "startOverride", &[("val", &s.to_string())])?;
    }
    place(d, n, false)
}

/// numIds paragraphs of the main part use directly.
fn used_nums(doc: &WordDoc) -> HashSet<i64> {
    let mut out = HashSet::new();
    for part in doc.part_indices() {
        let d = doc.part(part);
        for n in d.descendants(d.root()) {
            if is_w(d, n, "numPr")
                && let Some(id) = child_int(d, n, "numId")
            {
                out.insert(id);
            }
        }
    }
    out
}

/// A numId for a new list of `kind`: bullets share an existing bullet list's num; a numbered list
/// gets its own num (restarting at 1) on an existing decimal definition, or a new definition.
fn num_for(doc: &mut WordDoc, kind: Kind) -> OpResult<i64> {
    let used = used_nums(doc);
    let np = ensure_numbering(doc)?;
    let d = doc.part_mut(np);
    let defs = defs(d);
    let mut candidates: Vec<(i64, i64, bool)> = defs
        .nums
        .iter()
        .filter(|(_, (aid, _))| defs.abstract_fmt.get(aid).is_some_and(|f| kind.matches(f)))
        .map(|(&id, &(aid, ov))| (id, aid, ov))
        .collect();
    // Prefer lists in use, then plain (override-free) nums, then the lowest id.
    candidates.sort_by_key(|&(id, _, ov)| (!used.contains(&id), ov, id));
    match kind {
        Kind::Bullet => {
            if let Some(&(id, _, _)) = candidates.iter().find(|c| !c.2) {
                return Ok(id);
            }
            let aid = match candidates.first() {
                Some(&(_, aid, _)) => aid,
                None => {
                    let aid = defs.max_abstract + 1;
                    mint_abstract(d, aid, kind)?;
                    aid
                }
            };
            let id = defs.max_num + 1;
            mint_num(d, id, aid, None)?;
            Ok(id)
        }
        Kind::Numbered => {
            let id = defs.max_num + 1;
            match candidates.first() {
                // A fresh num on a shared definition continues its counter unless restarted.
                Some(&(_, aid, _)) => mint_num(d, id, aid, Some((0, 1)))?,
                None => {
                    let aid = defs.max_abstract + 1;
                    mint_abstract(d, aid, kind)?;
                    mint_num(d, id, aid, None)?;
                }
            }
            Ok(id)
        }
    }
}

/// Write `numPr` (`ilvl` and / or `numId`) on a paragraph whose `pPr` is already snapshotted.
fn set_num_pr(d: &mut Document, ppr: NodeId, num_id: Option<i64>, ilvl: Option<u32>) -> crate::Result<()> {
    let np = tw::ensure_child(d, ppr, "numPr", PPR_ORDER)?;
    if let Some(l) = ilvl {
        let el = tw::ensure_child(d, np, "ilvl", NUMPR_ORDER)?;
        tw::set_w(d, el, "val", &l.to_string())?;
    }
    if let Some(n) = num_id {
        let el = tw::ensure_child(d, np, "numId", NUMPR_ORDER)?;
        tw::set_w(d, el, "val", &n.to_string())?;
    }
    Ok(())
}

/// The list format (`bullet`, `decimal`, …) of a numId's level 0.
fn fmt_of(doc: &WordDoc, num_id: i64) -> Option<String> {
    let np = doc.numbering_part()?;
    let defs = defs(doc.part(np));
    let (aid, _) = defs.nums.get(&num_id)?;
    defs.abstract_fmt.get(aid).cloned()
}

#[derive(Deserialize)]
struct SetListArgs {
    anchor: String,
    list: String,
    #[serde(default)]
    level: Option<u32>,
}

/// `set_list {anchor, list: bullet|numbered|none|continue:<anchor>, level?}`.
pub fn set_list(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: SetListArgs = args(cx.index, op)?;
    if a.level.is_some_and(|l| l > 8) {
        return Err(OpError::invalid(cx.path("level"), "level is 0 to 8"));
    }
    let r = resolve(cx.doc, cx.anchors, cx.index, "anchor", &a.anchor)?;
    if r.paras.is_empty() {
        return Err(OpError::invalid(cx.path("anchor"), format!("{} holds no paragraph", a.anchor)));
    }
    {
        let d = cx.doc.part(r.part);
        if r.paras.iter().any(|&p| in_locked_control(d, p)) {
            return Err(locked_error(&a.anchor));
        }
    }
    let sheet = cx.doc.style_sheet()?;
    // (numId, ilvl) to write; None = take the list off.
    let target: Option<(i64, u32)> = match a.list.as_str() {
        "none" => None,
        "bullet" | "numbered" => {
            let kind = if a.list == "bullet" { Kind::Bullet } else { Kind::Numbered };
            let level = a.level.unwrap_or(0);
            // Right after a list of the same kind: join it.
            let prev = {
                let d = cx.doc.part(r.part);
                let mut cur = d.prev_sibling(r.paras[0]);
                while let Some(c) = cur {
                    if is_w(d, c, "p") || is_w(d, c, "tbl") {
                        break;
                    }
                    cur = d.prev_sibling(c);
                }
                cur.filter(|&c| is_w(d, c, "p"))
            };
            let numbering = Numbering::load(cx.doc);
            let join = prev
                .and_then(|p| numbering.effective(cx.doc.part(r.part), Paragraph(p)))
                .filter(|e| fmt_of(cx.doc, e.num_id).is_some_and(|f| kind.matches(&f)))
                .map(|e| e.num_id);
            let num = match join {
                Some(n) => n,
                None => num_for(cx.doc, kind)?,
            };
            Some((num, level))
        }
        other => match other.strip_prefix("continue:") {
            Some(h) if !h.trim().is_empty() => {
                let h = h.trim();
                let rc = resolve(cx.doc, cx.anchors, cx.index, "list", h)?;
                let Some(p) = rc.single_para() else {
                    return Err(OpError::invalid(cx.path("list"), format!("{h} is not a paragraph")));
                };
                let numbering = Numbering::load(cx.doc);
                let Some(e) = numbering.effective(cx.doc.part(rc.part), Paragraph(p)) else {
                    return Err(OpError::invalid(cx.path("list"), format!("{h} is not a list item")));
                };
                Some((e.num_id, a.level.unwrap_or(e.ilvl as u32)))
            }
            _ => return Err(OpError::invalid(cx.path("list"), "list is bullet, numbered, none or continue:<anchor>")),
        },
    };
    let mut tracker = Tracker::new(cx.anchors);
    let rev0 = cx.tk.revisions.len();
    let index = cx.index;
    let d = cx.doc.part_mut(r.part);
    for &p in &r.paras {
        track_ppr(d, cx.tk, p)?;
        match target {
            None => apply_list(d, sheet.as_ref(), p, &ListArg::None, index)?,
            Some((num, lvl)) => {
                let ppr = tw::ensure_ppr(d, p)?;
                set_num_pr(d, ppr, Some(num), Some(lvl))?;
            }
        }
    }
    for &p in &r.paras {
        tracker.touch(cx.doc, r.part, p);
    }
    Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()))
}

#[derive(Deserialize)]
struct RestartArgs {
    anchor: String,
    start: i64,
}

/// `restart_numbering {anchor, start}`: this item and the rest of its list move to a new `w:num`
/// that restarts at `start`.
pub fn restart_numbering(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: RestartArgs = args(cx.index, op)?;
    if a.start < 1 {
        return Err(OpError::invalid(cx.path("start"), "start is 1 or more"));
    }
    let r = resolve(cx.doc, cx.anchors, cx.index, "anchor", &a.anchor)?;
    let Some(p) = r.single_para() else {
        return Err(OpError::invalid(cx.path("anchor"), format!("{} is not a paragraph", a.anchor)));
    };
    if in_locked_control(cx.doc.part(r.part), p) {
        return Err(locked_error(&a.anchor));
    }
    let numbering = Numbering::load(cx.doc);
    let Some(e) = numbering.effective(cx.doc.part(r.part), Paragraph(p)) else {
        return Err(OpError::invalid(cx.path("anchor"), format!("{} is not a list item", a.anchor)));
    };
    // The rest of the list: following paragraphs of the same story with the same numId.
    let members: Vec<(NodeId, usize)> = {
        let d = cx.doc.part(r.part);
        let root = d.find_child(d.root(), ns::W, "body").unwrap_or(d.root());
        let all = paras_in(d, root);
        let from = all.iter().position(|&x| x == p).unwrap_or(0);
        all[from..]
            .iter()
            .filter_map(|&q| numbering.effective(d, Paragraph(q)).filter(|f| f.num_id == e.num_id).map(|f| (q, f.ilvl)))
            .collect()
    };
    let np = ensure_numbering(cx.doc)?;
    let new_id = {
        let d = cx.doc.part_mut(np);
        let defs = defs(d);
        let Some(&(aid, _)) = defs.nums.get(&e.num_id) else {
            return Err(OpError::invalid(cx.path("anchor"), format!("{}: its list definition is missing", a.anchor)));
        };
        let id = defs.max_num + 1;
        mint_num(d, id, aid, Some((e.ilvl, a.start)))?;
        id
    };
    let mut tracker = Tracker::new(cx.anchors);
    let rev0 = cx.tk.revisions.len();
    let d = cx.doc.part_mut(r.part);
    for &(q, ilvl) in &members {
        let ppr = track_ppr(d, cx.tk, q)?;
        set_num_pr(d, ppr, Some(new_id), Some(ilvl as u32))?;
    }
    for &(q, _) in &members {
        tracker.touch(cx.doc, r.part, q);
    }
    Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()))
}

#[derive(Deserialize)]
struct LevelArgs {
    anchor: String,
    #[serde(default)]
    delta: Option<i64>,
    #[serde(default)]
    level: Option<i64>,
}

/// `change_level {anchor, delta|level}`.
pub fn change_level(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: LevelArgs = args(cx.index, op)?;
    match (a.delta, a.level) {
        (Some(dl), None) if (-8..=8).contains(&dl) => {}
        (None, Some(l)) if (0..=8).contains(&l) => {}
        (Some(_), Some(_)) => return Err(OpError::invalid(cx.path("level"), "give delta or level, not both")),
        (Some(_), None) => return Err(OpError::invalid(cx.path("delta"), "delta is -8 to 8")),
        (None, Some(_)) => return Err(OpError::invalid(cx.path("level"), "level is 0 to 8")),
        (None, None) => return Err(OpError::invalid(cx.path("delta"), "give delta or level")),
    }
    let r = resolve(cx.doc, cx.anchors, cx.index, "anchor", &a.anchor)?;
    let numbering = Numbering::load(cx.doc);
    let levels: Vec<(NodeId, u32)> = {
        let d = cx.doc.part(r.part);
        let mut out = Vec::new();
        for &p in &r.paras {
            if in_locked_control(d, p) {
                return Err(locked_error(&a.anchor));
            }
            let Some(e) = numbering.effective(d, Paragraph(p)) else {
                return Err(OpError::invalid(
                    cx.path("anchor"),
                    format!("{} is not a list item; use set_list", a.anchor),
                ));
            };
            let new = match (a.delta, a.level) {
                (Some(dl), _) => (e.ilvl as i64 + dl).clamp(0, 8),
                (_, Some(l)) => l,
                _ => e.ilvl as i64,
            };
            if new as usize != e.ilvl {
                out.push((p, new as u32));
            }
        }
        out
    };
    let mut tracker = Tracker::new(cx.anchors);
    let rev0 = cx.tk.revisions.len();
    let d = cx.doc.part_mut(r.part);
    for &(p, l) in &levels {
        let ppr = track_ppr(d, cx.tk, p)?;
        set_num_pr(d, ppr, None, Some(l))?;
    }
    for &(p, _) in &levels {
        tracker.touch(cx.doc, r.part, p);
    }
    Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()))
}
