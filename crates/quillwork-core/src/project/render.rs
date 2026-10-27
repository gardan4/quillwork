//! The projection renderer (spec/projection.md): block lines, tables, images, story sections,
//! the header, outline and window lines, the appendix, selections and budgets.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;

use crate::anchors::{self, Anchors, Target};
use crate::wml::style::{EffectiveParagraph, StyleSheet};
use crate::wml::tree::{child_on, child_val, w_attr};
use crate::wml::view::{self, Block, Paragraph, Table, VMerge};
use crate::wml::{HfType, Loc, PartIdx, StoryKind, WordDoc};
use crate::xml::{Document, NodeId, ns};
use crate::{Error, Result};

use super::ctx::{self, DocCtx, DocIndex};
use super::image;
use super::inline::{self, Atom, Control, CtlValue, InlineState, Piece, RenderOpts, RevKind};
use super::select::{self, Sel};
use super::{DEFAULT_BUDGET, MIN_BUDGET, ProjectOptions, ReadMode, ReadRequest, ReadResult, View, estimate_tokens};

/// Outline lines longer than this drop headings below level 2.
const OUTLINE_MAX: usize = 1500;
/// Outline entry text is cut at this many characters.
const OUTLINE_ENTRY: usize = 80;
/// Quoted comment ranges are cut at this many characters.
const ON_TEXT: usize = 60;
/// At most this many entries per appendix line.
const APPENDIX_MAX: usize = 40;

/// Paragraph styles that never get a `STYLE=` kind: body-text styles and the default styles of
/// the non-body stories (lower-cased style names).
const BODY_LIKE: &[&str] = &[
    "normal",
    "default",
    "standard",
    "text body",
    "no spacing",
    "normal (web)",
    "plain text",
    "header",
    "footer",
    "footnote text",
    "endnote text",
    "annotation text",
    "comment text",
    "table contents",
    "table paragraph",
];

// ------------------------------------------------------------------------------- units

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitKind {
    Para,
    Table,
    Fold,
    Toc,
}

/// One top-level block (or a fold of empty paragraphs, or a TOC) as rendered lines.
#[derive(Debug, Clone)]
struct Unit {
    kind: UnitKind,
    /// `§ section` line printed before the unit, if a section starts here.
    section: Option<String>,
    lines: Vec<String>,
    /// First anchor printed.
    head: String,
    /// Last anchor printed (the window line's end).
    tail: String,
    /// Top-level nodes covered (to map anchors to units).
    nodes: Vec<NodeId>,
    /// Top-level body blocks covered.
    blocks: usize,
    /// A bare empty paragraph (foldable).
    empty: bool,
    heading: Option<u8>,
    /// Table units: (index into `lines`, 1-based row number) of each row's first line.
    rows: Vec<(usize, usize)>,
}

impl Unit {
    fn all_lines(&self) -> Vec<String> {
        let mut v = Vec::with_capacity(self.lines.len() + 1);
        if let Some(s) = &self.section {
            v.push(s.clone());
        }
        v.extend(self.lines.iter().cloned());
        v
    }
}

/// One rendered paragraph.
#[derive(Debug, Clone)]
struct ParaOut {
    /// The anchor the line starts with (the image anchor for an image line).
    head: String,
    line: String,
    /// Rendered inline text alone.
    text: String,
    empty: bool,
    heading: Option<u8>,
}

// ------------------------------------------------------------------------------- collect

/// Appendix material gathered while rendering.
#[derive(Default)]
struct Collect {
    refs: Vec<String>,
    fields: Vec<String>,
    /// The same refs, controls and placeholders as data ([`fields`]).
    found: Fields,
    on_text: HashMap<i64, String>,
    on_last: HashMap<i64, NodeId>,
    terms: Vec<(String, String)>,
    term_seen: HashSet<String>,
}

/// Fillable fields and cross-references of a document ([`fields`]): what `list_fields` and
/// `lint` report, gathered by the same pass as the projection's appendix.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fields {
    /// Content controls, in document order (block-level ones only when tagged or named).
    pub controls: Vec<ControlField>,
    /// Placeholder text (`{naam}`, `[NAAM]`, `…….`) in the accepted text.
    pub placeholders: Vec<Placeholder>,
    /// REF / PAGEREF / NOTEREF fields (not the `_Toc` ones of a table of contents).
    pub refs: Vec<RefField>,
}

/// A content control (`w:sdt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlField {
    /// The paragraph it sits in (or the first paragraph inside it).
    pub anchor: String,
    pub tag: Option<String>,
    pub alias: Option<String>,
    /// View-model `ContentControl.type`.
    pub typ: String,
    /// Its text; `None` when empty or showing its placeholder, and for a checkbox.
    pub value: Option<String>,
    /// A checkbox's state.
    pub checked: Option<bool>,
    pub locked: bool,
    pub bound: bool,
    /// A dropdown's or combo box's choices.
    pub items: Vec<String>,
}

impl ControlField {
    fn of(c: &Control, anchor: &str) -> ControlField {
        let (value, checked) = match &c.value {
            CtlValue::Text(v) => (Some(v.clone()), None),
            CtlValue::Empty => (None, None),
            CtlValue::Check(on) => (None, Some(*on)),
        };
        ControlField {
            anchor: anchor.to_owned(),
            tag: c.tag.clone(),
            alias: c.alias.clone(),
            typ: c.typ.to_owned(),
            value,
            checked,
            locked: c.locked,
            bound: c.bound,
            items: c.items.clone(),
        }
    }
}

/// Placeholder text left to fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placeholder {
    pub anchor: String,
    pub text: String,
}

/// A cross-reference field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefField {
    /// The block the field sits in.
    pub anchor: String,
    /// `REF`, `PAGEREF` or `NOTEREF`.
    pub typ: String,
    pub bookmark: String,
    /// The block the bookmark sits in; `None` when the bookmark is gone (a broken ref).
    pub target: Option<String>,
    /// The field's cached result.
    pub result: String,
}

fn term_res() -> &'static [Regex; 2] {
    static R: OnceLock<[Regex; 2]> = OnceLock::new();
    R.get_or_init(|| {
        [
            Regex::new(r#"(?i)\b(?:hierna(?:\s+te\s+noemen)?|hereinafter(?:\s+(?:referred\s+to\s+as|called))?)\s*[:,]?\s*["“”'‘’]([^"“”'‘’]{1,60})["“”'‘’]"#)
                .expect("term regex"),
            Regex::new(r#"\(\s*["“]([^"“”]{1,60})["”]\s*\)"#).expect("term regex"),
        ]
    })
}

fn placeholder_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\{[\p{L}_][\p{L}\p{N}_ .\-]{0,40}\}|\[[\p{Lu}][\p{Lu}\p{N}_ .\-]{1,40}\]|[.…_]{5,}")
            .expect("placeholder regex")
    })
}

// ------------------------------------------------------------------------------- renderer

pub(crate) struct Renderer<'c, 'a> {
    cx: &'c DocCtx<'a>,
    col: Collect,
    collecting: bool,
    default_style: Option<String>,
}

impl<'c, 'a> Renderer<'c, 'a> {
    pub(crate) fn new(cx: &'c DocCtx<'a>) -> Self {
        let default_style = cx.sheet.default_paragraph_style_id().map(str::to_owned);
        Renderer { cx, col: Collect::default(), collecting: false, default_style }
    }

    fn d(&self, part: PartIdx) -> &'a Document {
        self.cx.doc.part(part)
    }

    // -------------------------------------------------------------------------- paragraphs

    /// `TITLE`, `H2`, `STYLE=Foo`, … or none.
    pub(crate) fn kind(&self, part: PartIdx, p: NodeId) -> Option<String> {
        let ep = self.cx.para(part, p);
        let sid = ep.style.clone().or_else(|| self.default_style.clone());
        let style = sid.as_deref().and_then(|s| self.cx.sheet.style_by_id(s));
        let name = style
            .and_then(|s| s.name())
            .map(str::to_owned)
            .unwrap_or_else(|| sid.clone().unwrap_or_default())
            .to_lowercase();
        match name.as_str() {
            "title" => return Some("TITLE".into()),
            "quote" | "intense quote" => return Some("QUOTE".into()),
            "caption" => return Some("CAPTION".into()),
            _ => {}
        }
        if let Some(rest) = name.strip_prefix("toc ")
            && rest.trim().parse::<u8>().is_ok()
        {
            return Some("TOC".into());
        }
        if let Some(l) = ep.outline_level
            && l < 9
        {
            return Some(format!("H{}", l + 1));
        }
        if let Some(rest) = name.strip_prefix("heading ")
            && let Ok(n) = rest.trim().parse::<u8>()
            && (1..=9).contains(&n)
        {
            return Some(format!("H{n}"));
        }
        let sid = sid?;
        if Some(&sid) == self.default_style.as_ref()
            || BODY_LIKE.contains(&name.as_str())
            || name.starts_with("list")
            || name.starts_with("body text")
        {
            return None;
        }
        Some(format!("STYLE={sid}"))
    }

    /// Heading level (1–9) of a paragraph, if it is one.
    pub(crate) fn heading_level(&self, part: PartIdx, p: NodeId) -> Option<u8> {
        let k = self.kind(part, p)?;
        if k == "TITLE" {
            return Some(1);
        }
        k.strip_prefix('H').and_then(|n| n.parse().ok())
    }

    /// `(label text, level)` of a numbered or bulleted paragraph.
    pub(crate) fn label_parts(&self, part: PartIdx, p: NodeId) -> Option<(String, usize)> {
        let l = self.cx.label(Loc::new(part, p))?;
        let mut t = l.text.trim().to_owned();
        if t.is_empty() {
            return None;
        }
        if l.num_fmt == "bullet" {
            let mut cs = t.chars();
            let c = cs.next();
            let single = cs.next().is_none();
            t = match c {
                Some(
                    c @ ('•' | '–' | '—' | '-' | '▪' | '■' | '□' | '○' | '◦' | '●' | '➢' | '►' | '✓' | '✔' | '*' | '·'),
                ) if single => c.to_string(),
                Some('o') if single => "◦".into(),
                _ => "•".into(),
            };
        }
        Some((t, l.ilvl))
    }

    fn label(&self, part: PartIdx, p: NodeId) -> Option<String> {
        let (t, ilvl) = self.label_parts(part, p)?;
        Some(if ilvl > 0 { format!("#{t} L{ilvl}") } else { format!("#{t}") })
    }

    fn collect(&mut self, part: PartIdx, p: NodeId, anchor: &str, pieces: &[Piece], plain: &str) {
        let cx = self.cx;
        for pc in pieces {
            for &id in pc.comments.iter() {
                if pc.plain.is_empty() {
                    continue;
                }
                let s = self.col.on_text.entry(id).or_default();
                let last = self.col.on_last.insert(id, p);
                if last.is_some_and(|l| l != p) && !s.is_empty() {
                    s.push(' ');
                }
                if s.chars().count() <= ON_TEXT + 1 {
                    s.push_str(&pc.plain);
                }
            }
            match &pc.atom {
                Atom::Field(f) => {
                    let typ = f.typ();
                    if matches!(typ.as_str(), "REF" | "PAGEREF" | "NOTEREF") {
                        let bm = f.args().split_whitespace().find(|a| !a.starts_with('\\')).unwrap_or("").to_owned();
                        if bm.starts_with("_Toc") {
                            continue;
                        }
                        let target = cx.bookmarks.get(&bm).map(String::as_str).unwrap_or("?");
                        self.col
                            .refs
                            .push(format!("{typ} {bm} in {anchor} → {target} \"{}\"", inline::quote(&f.result)));
                        self.col.found.refs.push(RefField {
                            anchor: anchor.to_owned(),
                            typ: typ.clone(),
                            bookmark: bm.clone(),
                            target: cx.bookmarks.get(&bm).cloned(),
                            result: f.result.clone(),
                        });
                    }
                }
                Atom::Control(c) => {
                    let e = control_entry(c, anchor);
                    self.col.fields.push(e);
                    self.col.found.controls.push(ControlField::of(c, anchor));
                }
                _ => {}
            }
        }
        let _ = part;
        if plain.is_empty() {
            return;
        }
        for m in placeholder_re().find_iter(plain) {
            self.col.fields.push(format!("\"{}\" placeholder in {anchor}", inline::quote(m.as_str())));
            self.col.found.placeholders.push(Placeholder { anchor: anchor.to_owned(), text: m.as_str().to_owned() });
        }
        for re in term_res() {
            for c in re.captures_iter(plain) {
                let t = c[1].trim().to_owned();
                if !t.is_empty() && self.col.term_seen.insert(t.to_lowercase()) {
                    self.col.terms.push((t, anchor.to_owned()));
                }
            }
        }
    }

    /// Render one paragraph. `None` when it disappears in this view (a deleted paragraph mark
    /// with nothing left to show).
    fn para(&mut self, part: PartIdx, p: NodeId, st: &mut InlineState, in_table: bool) -> Option<ParaOut> {
        let cx = self.cx;
        let d = self.d(part);
        let pieces = inline::pieces(cx, part, p, st);
        let anchor = cx.anchors.handle_of(Loc::new(part, p)).unwrap_or("?").to_owned();
        if self.collecting {
            let plain: String = pieces.iter().map(|x| x.plain.as_str()).collect();
            self.collect(part, p, &anchor, &pieces, &plain);
        }
        let has_mark = pieces.iter().any(|x| matches!(x.atom, Atom::ParaMark));
        // A paragraph that holds only one drawing: an image line.
        let mut img = None;
        let image_only = pieces.iter().all(|x| match &x.atom {
            Atom::Image(n) => {
                let first = img.is_none();
                img = Some(*n);
                first && x.rev.is_none() && x.fence.is_none()
            }
            Atom::Text(t) => t.trim().is_empty() && x.rev.is_none(),
            _ => false,
        });
        if image_only
            && let Some(n) = img
            && let Some(ia) = cx.anchors.handle_of(Loc::new(part, n))
        {
            let line = image_line(cx, part, n, ia);
            return Some(ParaOut { head: ia.to_owned(), text: line.clone(), line, empty: false, heading: None });
        }
        let text = inline::render(cx, part, &pieces, RenderOpts { in_table });
        let mark_del = inline::mark_rev(d, p).is_some_and(|r| r.kind.is_del());
        let empty = text.trim().is_empty();
        if empty && mark_del && !has_mark {
            return None;
        }
        let kind = self.kind(part, p);
        let label = self.label(part, p);
        let heading = match kind.as_deref() {
            Some("TITLE") => Some(1),
            Some(k) => k.strip_prefix('H').and_then(|n| n.parse().ok()),
            None => None,
        };
        let mut line = String::with_capacity(text.len() + anchor.len() + 16);
        line.push('[');
        line.push_str(&anchor);
        for x in [&kind, &label].into_iter().flatten() {
            line.push(' ');
            line.push_str(x);
        }
        line.push(']');
        if !empty {
            line.push(' ');
            line.push_str(&text);
        }
        Some(ParaOut { head: anchor, line, text, empty: empty && kind.is_none() && label.is_none(), heading })
    }

    // -------------------------------------------------------------------------- tables

    /// Render a table. Lines are not indented; nested tables inside expanded cells are.
    fn table(&mut self, part: PartIdx, t: Table, st: &mut InlineState) -> Unit {
        let cx = self.cx;
        let d = self.d(part);
        let ta = cx.anchors.handle_of(Loc::new(part, t.0)).unwrap_or("t?").to_owned();
        let rows = t.rows(d);
        let mut lines = vec![String::new()];
        let mut row_starts = Vec::new();
        let mut max_span = 0u32;
        for (ri, row) in rows.iter().enumerate() {
            let rno = ri + 1;
            let cells = row.cells(d);
            max_span = max_span.max(cells.iter().map(|c| c.grid_span(d).max(1)).sum());
            // Row revision.
            let rrev = row.revision(d).and_then(|rv| {
                let kind = match rv.kind {
                    view::RevisionKind::RowIns => RevKind::Ins,
                    view::RevisionKind::RowDel => RevKind::Del,
                    _ => return None,
                };
                Some((kind, rv.author(d).unwrap_or_default(), rv.id(d)))
            });
            let shown = rrev.as_ref().map(|(k, a, id)| (*k, a.clone(), *id, cx.shows(a, *id)));
            if let Some((k, _, _, false)) = &shown
                && k.is_del()
            {
                // Deleted row, accepted silently: still walk it for comment state.
                for c in &cells {
                    for p in c.paragraphs(d) {
                        let _ = inline::pieces(cx, part, p.0, st);
                    }
                }
                continue;
            }
            let wrap = shown.filter(|s| s.3).map(|(k, a, id, _)| (k, a, id));
            let hdr = row_tr_pr(d, row.0).is_some_and(|tp| child_on(d, tp, "tblHeader"));
            let mut head = format!("  [{ta}.r{rno}");
            if hdr {
                head.push_str(" hdr");
            }
            head.push(']');
            let compact = cells.iter().all(|c| {
                let b = c.blocks(d);
                b.len() == 1
                    && matches!(b[0], Block::Paragraph(p) if self.label_parts(part, p.0).is_none() && !has_drawing(d, p.0))
            });
            row_starts.push((lines.len(), rno));
            if compact {
                let mut parts = Vec::with_capacity(cells.len());
                for c in &cells {
                    let Some(Block::Paragraph(p)) = c.blocks(d).into_iter().next() else { continue };
                    let mut pieces = inline::pieces(cx, part, p.0, st);
                    if self.collecting {
                        let anchor = cx.anchors.handle_of(Loc::new(part, p.0)).unwrap_or("?").to_owned();
                        let plain: String = pieces.iter().map(|x| x.plain.as_str()).collect();
                        self.collect(part, p.0, &anchor, &pieces, &plain);
                    }
                    if let Some((k, a, _)) = &wrap {
                        strip_rev(&mut pieces, *k, a);
                    }
                    let mut s = if c.v_merge(d) == VMerge::Continue {
                        "^".to_owned()
                    } else {
                        inline::render(cx, part, &pieces, RenderOpts { in_table: true }).trim().to_owned()
                    };
                    for _ in 1..c.grid_span(d).max(1) {
                        s.push_str(" |>");
                    }
                    parts.push(s);
                }
                let body = parts.join(" | ");
                let mut line = head;
                match &wrap {
                    Some((k, a, id)) => {
                        let m = if k.is_del() { "--" } else { "++" };
                        line.push_str(&format!(" {{{m}{body}{m}}}{}", rev_tag(cx, a, *id)));
                    }
                    None if !body.trim().is_empty() => {
                        line.push(' ');
                        line.push_str(&body);
                    }
                    None => {}
                }
                lines.push(line);
            } else {
                let mut line = head;
                if let Some((k, a, id)) = &wrap {
                    let m = if k.is_del() { "--" } else { "++" };
                    line.push_str(&format!(" {{{m}row{m}}}{}", rev_tag(cx, a, *id)));
                }
                lines.push(line);
                for (ci, c) in cells.iter().enumerate() {
                    if ci > 0 {
                        lines.push("    ||".into());
                    }
                    if c.v_merge(d) == VMerge::Continue {
                        lines.push("    ^".into());
                        for p in c.paragraphs(d) {
                            let _ = inline::pieces(cx, part, p.0, st);
                        }
                        continue;
                    }
                    let mut sub = Vec::new();
                    self.blocks_into(part, &c.blocks(d), st, true, &mut sub);
                    for l in sub {
                        lines.push(format!("    {l}"));
                    }
                }
            }
        }
        let cols = grid_cols(d, t.0).unwrap_or(max_span as usize);
        let caption = tbl_pr(d, t.0).and_then(|tp| child_val(d, tp, "tblCaption")).filter(|c| !c.trim().is_empty());
        let mut h = format!("[{ta} TABLE {}×{cols}", row_starts.len());
        if let Some(c) = caption {
            h.push_str(&format!(" \"{}\"", inline::quote(&c)));
        }
        h.push(']');
        lines[0] = h;
        Unit {
            kind: UnitKind::Table,
            section: None,
            lines,
            head: ta.clone(),
            tail: ta,
            nodes: vec![t.0],
            blocks: 1,
            empty: false,
            heading: None,
            rows: row_starts,
        }
    }

    /// Render blocks (cell content, nested) as plain lines.
    fn blocks_into(
        &mut self,
        part: PartIdx,
        blocks: &[Block],
        st: &mut InlineState,
        in_table: bool,
        out: &mut Vec<String>,
    ) {
        for b in blocks {
            match *b {
                Block::Paragraph(p) => {
                    if let Some(o) = self.para(part, p.0, st, in_table) {
                        out.push(o.line);
                    }
                }
                Block::Table(t) => {
                    let u = self.table(part, t, st);
                    out.extend(u.lines);
                }
                Block::Sdt(s) => {
                    let d = self.d(part);
                    if self.collecting {
                        self.block_control(part, s.0);
                    }
                    let inner = s.blocks(d);
                    self.blocks_into(part, &inner, st, in_table, out);
                }
            }
        }
    }

    fn block_control(&mut self, part: PartIdx, sdt: NodeId) {
        let d = self.d(part);
        let c = Control::of(d, sdt);
        if c.tag.is_none() && c.alias.is_none() {
            return;
        }
        let anchor = view::Sdt(sdt)
            .content(d)
            .and_then(|ct| d.descendants(ct).find(|&n| d.is(n, ns::W, "p")))
            .and_then(|p| self.cx.anchors.handle_of(Loc::new(part, p)))
            .unwrap_or("?")
            .to_owned();
        let e = control_entry(&c, &anchor);
        self.col.fields.push(e);
        self.col.found.controls.push(ControlField::of(&c, &anchor));
    }

    // -------------------------------------------------------------------------- stories

    /// Top-level blocks of a story with block-level content controls flattened.
    fn flat_blocks(&mut self, part: PartIdx, root: NodeId) -> Vec<Block> {
        let d = self.d(part);
        let mut out = Vec::new();
        let mut stack: Vec<std::vec::IntoIter<Block>> = vec![view::blocks(d, root).into_iter()];
        while let Some(it) = stack.last_mut() {
            match it.next() {
                None => {
                    stack.pop();
                }
                Some(Block::Sdt(s)) => {
                    if self.collecting {
                        self.block_control(part, s.0);
                    }
                    stack.push(s.blocks(d).into_iter());
                }
                Some(b) => out.push(b),
            }
        }
        out
    }

    /// The units of story `si`.
    fn story_units(&mut self, si: usize) -> Vec<Unit> {
        let story = self.cx.story(si);
        let flat = self.flat_blocks(story.part, story.root);
        self.units_of(si, &flat, 0, InlineState::default(), None, |_, _| false)
    }

    /// The units of story `si` (whose flattened blocks are `flat`) from block `start`, until
    /// `stop(next block index, units so far)` says so. `st` and `pending_section` are the
    /// state at `start` (see [`Self::window_start`]).
    fn units_of(
        &mut self,
        si: usize,
        flat: &[Block],
        start: usize,
        mut st: InlineState,
        mut pending_section: Option<String>,
        mut stop: impl FnMut(usize, &[Unit]) -> bool,
    ) -> Vec<Unit> {
        let cx = self.cx;
        let story = cx.story(si);
        let part = story.part;
        let d = self.d(part);
        let body = matches!(story.kind, StoryKind::Body);
        let mut units: Vec<Unit> = Vec::with_capacity(flat.len().saturating_sub(start).min(1024));
        let mut i = start;
        while i < flat.len() {
            if stop(i, &units) {
                break;
            }
            match flat[i] {
                Block::Paragraph(p) => {
                    if let Some(instr) = toc_instr(d, p.0) {
                        let end = toc_end(d, flat, i);
                        let paras: Vec<NodeId> = flat[i..=end]
                            .iter()
                            .filter_map(|b| if let Block::Paragraph(p) = b { Some(p.0) } else { None })
                            .collect();
                        let entries =
                            paras.iter().filter(|&&p| !cx.accepted_text(part, Paragraph(p)).trim().is_empty()).count();
                        let anchors: Vec<String> = paras
                            .iter()
                            .filter_map(|&p| cx.anchors.handle_of(Loc::new(part, p)).map(str::to_owned))
                            .collect();
                        let head = anchors.first().cloned().unwrap_or_else(|| "?".into());
                        let args = instr.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
                        let sep = if args.is_empty() { "" } else { " " };
                        let line = format!(
                            "[{head} TOC] ⟪TOC{sep}{args} → {entries} {}⟫",
                            if entries == 1 { "entry" } else { "entries" }
                        );
                        units.push(Unit {
                            kind: UnitKind::Toc,
                            section: pending_section.take(),
                            lines: vec![line],
                            tail: head.clone(),
                            head,
                            nodes: flat[i..=end].iter().map(|b| b.node()).collect(),
                            blocks: end + 1 - i,
                            empty: false,
                            heading: None,
                            rows: Vec::new(),
                        });
                        if body {
                            for b in &flat[i..=end] {
                                if let Block::Paragraph(p) = b
                                    && let Some(s) = self.section_after(part, p.0)
                                {
                                    pending_section = Some(s);
                                }
                            }
                        }
                        i = end + 1;
                        continue;
                    }
                    if let Some(o) = self.para(part, p.0, &mut st, false) {
                        units.push(Unit {
                            kind: UnitKind::Para,
                            section: pending_section.take(),
                            lines: vec![o.line],
                            head: o.head.clone(),
                            tail: o.head,
                            nodes: vec![p.0],
                            blocks: 1,
                            empty: o.empty,
                            heading: o.heading,
                            rows: Vec::new(),
                        });
                    } else if let Some(last) = units.last_mut() {
                        // Merged into the previous unit's block count.
                        last.nodes.push(p.0);
                        last.blocks += 1;
                    }
                    if body && let Some(s) = self.section_after(part, p.0) {
                        pending_section = Some(s);
                    }
                }
                Block::Table(t) => {
                    let mut u = self.table(part, t, &mut st);
                    u.section = pending_section.take();
                    units.push(u);
                }
                Block::Sdt(_) => {}
            }
            i += 1;
        }
        fold_empties(units)
    }

    /// The `§ section` line for the section that starts after paragraph `p`, if `p` ends one.
    fn section_after(&self, part: PartIdx, p: NodeId) -> Option<String> {
        let d = self.d(part);
        let ppr = Paragraph(p).ppr(d)?;
        let sp = d.find_child(ppr, ns::W, "sectPr")?;
        let secs = self.cx.doc.sections();
        let k = secs.iter().position(|s| s.sect_pr == sp)?;
        let next = secs.get(k + 1)?;
        Some(section_line(d, next.sect_pr, k + 2))
    }

    /// Where a window that must show flattened block `from` of story `si` starts rendering
    /// (the head of the TOC group `from` sits in, else `from`), with the inline state and the
    /// pending `§ section` line at that block. Scans the blocks before it without rendering.
    fn window_start(&self, si: usize, flat: &[Block], from: usize) -> (usize, InlineState, Option<String>) {
        let cx = self.cx;
        let story = cx.story(si);
        let part = story.part;
        let d = self.d(part);
        let mut st = InlineState::default();
        let mut start = from.min(flat.len());
        let mut k = 0;
        while k < start {
            if let Block::Paragraph(p) = flat[k]
                && toc_instr(d, p.0).is_some()
            {
                let end = toc_end(d, flat, k);
                if end >= from {
                    start = k;
                    break;
                }
                for b in &flat[k..=end] {
                    st.skip(cx, d, b.node());
                }
                k = end + 1;
                continue;
            }
            st.skip(cx, d, flat[k].node());
            k += 1;
        }
        let pending = match start.checked_sub(1).map(|k| flat[k]) {
            Some(Block::Paragraph(p)) if matches!(story.kind, StoryKind::Body) => self.section_after(part, p.0),
            _ => None,
        };
        (start, st, pending)
    }

    fn story_line(&self, si: usize) -> String {
        let cx = self.cx;
        let sid = cx.story_id(si);
        let slots = |refs: &[crate::wml::HfRef]| {
            let s: Vec<String> = refs
                .iter()
                .map(|r| {
                    let slot = match r.hf {
                        HfType::Default => "default",
                        HfType::First => "first",
                        HfType::Even => "even",
                    };
                    format!("s{} {slot}", r.section + 1)
                })
                .collect();
            if s.is_empty() { String::new() } else { format!(" · {}", s.join(", ")) }
        };
        match &cx.story(si).kind {
            StoryKind::Header { refs } => format!("§ {sid} header{}", slots(refs)),
            StoryKind::Footer { refs } => format!("§ {sid} footer{}", slots(refs)),
            StoryKind::Footnote { .. } => format!("§ {sid} footnote"),
            StoryKind::Endnote { .. } => format!("§ {sid} endnote"),
            StoryKind::TextBox { .. } => format!("§ {sid} textbox"),
            StoryKind::Comment { .. } => format!("§ {sid} comment"),
            StoryKind::Body => format!("§ {sid} body"),
        }
    }

    /// Story section (story line + lines), `None` when the story is empty.
    fn story_section(&mut self, si: usize) -> Option<Vec<String>> {
        let units = self.story_units(si);
        if units.iter().all(|u| u.empty || u.kind == UnitKind::Fold) {
            return None;
        }
        let mut out = vec![self.story_line(si)];
        for u in &units {
            out.extend(u.lines.iter().cloned());
        }
        Some(out)
    }

    /// Headers, footers and text boxes.
    fn stories(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for si in 0..self.cx.doc.stories().len() {
            if matches!(
                self.cx.story(si).kind,
                StoryKind::Header { .. } | StoryKind::Footer { .. } | StoryKind::TextBox { .. }
            ) && let Some(s) = self.story_section(si)
            {
                out.extend(s);
            }
        }
        out
    }

    /// Rendered inline text of each paragraph of a story (comments, notes).
    fn story_texts(&mut self, si: usize) -> Vec<String> {
        let story = self.cx.story(si);
        let part = story.part;
        let d = self.d(part);
        let mut st = InlineState::default();
        let mut out = Vec::new();
        for p in view::paragraphs(d, story.root) {
            if let Some(o) = self.para(part, p.0, &mut st, false) {
                out.push(o.text);
            }
        }
        out
    }

    // -------------------------------------------------------------------------- appendix

    fn comment_entry(&mut self, idx: usize, indent: &str, top: bool) -> String {
        let c = self.cx.comments[idx].clone();
        let text: Vec<String> = self.story_texts(c.story).into_iter().filter(|t| !t.trim().is_empty()).collect();
        let mut head = format!("{indent}[{} {}", c.sid, self.cx.author_label(&c.author));
        if top {
            head.push_str(if c.done { ", resolved" } else { ", open" });
            if let Some(on) = self.col.on_text.get(&c.wid).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) {
                head.push_str(&format!(", on \"{}\"", inline::quote(&cut_chars(&on, ON_TEXT))));
            }
        }
        head.push(']');
        let body = text.join(" ↵ ");
        if !body.is_empty() {
            head.push(' ');
            head.push_str(&body);
        }
        head
    }

    fn comment_lines(&mut self, idx: usize) -> Vec<String> {
        let sid = self.cx.comments[idx].sid.clone();
        let mut out = vec![self.comment_entry(idx, "  ", true)];
        let replies: Vec<usize> = (0..self.cx.comments.len())
            .filter(|&j| self.cx.comments[j].parent.as_deref() == Some(sid.as_str()))
            .collect();
        for j in replies {
            out.push(self.comment_entry(j, "    ", false));
        }
        out
    }

    fn appendix(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let join = |v: Vec<String>| {
            let n = v.len();
            let mut s: Vec<String> = v.into_iter().take(APPENDIX_MAX).collect();
            if n > APPENDIX_MAX {
                s.push(format!("… (+{})", n - APPENDIX_MAX));
            }
            s.join(" · ")
        };
        let terms: Vec<String> =
            self.col.terms.iter().map(|(t, a)| format!("\"{}\" → {a}", inline::quote(t))).collect();
        if !terms.is_empty() {
            out.push(format!("§ terms: {}", join(terms)));
        }
        let refs = std::mem::take(&mut self.col.refs);
        if !refs.is_empty() {
            out.push(format!("§ refs: {}", join(refs)));
        }
        let mut fields = std::mem::take(&mut self.col.fields);
        let mut seen = HashSet::new();
        fields.retain(|f| seen.insert(f.clone()));
        if !fields.is_empty() {
            out.push(format!("§ fields: {}", join(fields)));
        }
        // Comments.
        let tops: Vec<usize> = (0..self.cx.comments.len()).filter(|&i| self.cx.comments[i].parent.is_none()).collect();
        let open: Vec<usize> = tops.iter().copied().filter(|&i| !self.cx.comments[i].done).collect();
        let resolved = tops.len() - open.len();
        if open.is_empty() {
            if resolved > 0 {
                out.push(format!("§ comments: {resolved} resolved (list_comments)"));
            }
        } else {
            out.push("§ comments:".into());
            for i in open {
                out.extend(self.comment_lines(i));
            }
            if resolved > 0 {
                out.push(format!("  {resolved} resolved (list_comments)"));
            }
        }
        // Notes.
        for (endnote, title) in [(false, "§ footnotes:"), (true, "§ endnotes:")] {
            let mut lines = Vec::new();
            for si in 0..self.cx.doc.stories().len() {
                let ok = match self.cx.story(si).kind {
                    StoryKind::Footnote { .. } => !endnote,
                    StoryKind::Endnote { .. } => endnote,
                    _ => false,
                };
                if !ok {
                    continue;
                }
                let units = self.story_units(si);
                for u in units {
                    if u.empty {
                        continue;
                    }
                    for l in u.lines {
                        // The text after the note's own reference mark starts with a space or tab.
                        let l = match l.split_once("] ") {
                            Some((h, t)) if l.starts_with('[') => {
                                format!("{h}] {}", t.trim_start_matches([' ', '⇥', '\u{a0}']))
                            }
                            _ => l,
                        };
                        lines.push(format!("  {l}"));
                    }
                }
            }
            if !lines.is_empty() {
                out.push(title.into());
                out.extend(lines);
            }
        }
        out
    }

    // -------------------------------------------------------------------------- header

    fn header(&self, body_chars: usize, first_heading: Option<String>) -> String {
        let cx = self.cx;
        let doc = cx.doc;
        let title = doc_title(cx, first_heading).unwrap_or_else(|| "Untitled".into());
        let pages = doc_pages(cx, body_chars);
        let lang = self.lang().unwrap_or_else(|| "und".into());
        let track = if ctx::track_revisions(doc) { "on" } else { "off" };
        let open_comments = cx.comments.iter().filter(|c| c.parent.is_none() && !c.done).count();
        let (revs, foreign_del) = self.revision_counts();
        let mut flags: Vec<&str> = Vec::new();
        let seen = cx.flags.borrow();
        for f in ["hidden_text", "tiny_text", "white_text"] {
            if seen.contains(f) {
                flags.push(f);
            }
        }
        if foreign_del {
            flags.push("foreign_deletions");
        }
        let pf = package_flags(doc);
        if pf.macros {
            flags.push("macros");
        }
        if pf.external_links {
            flags.push("external_links");
        }
        if pf.protected {
            flags.push("protection");
        }
        if pf.signed {
            flags.push("signed");
        }
        let pl = |n: usize, w: &str| if n == 1 { format!("{n} {w}") } else { format!("{n} {w}s") };
        let mut s = format!(
            "§ doc \"{}\" · v{} · {} (est.) · {lang} · tracked changes: {track} · {} · {}",
            inline::quote(title.trim()),
            cx.opts.version_of(doc),
            pl(pages as usize, "page"),
            pl(open_comments, "open comment"),
            pl(revs, "open revision"),
        );
        if !flags.is_empty() {
            s.push_str(" · flags: ");
            s.push_str(&flags.join(","));
        }
        s.push_str(" · view: ");
        s.push_str(cx.view.as_str());
        s
    }

    pub(crate) fn lang(&self) -> Option<String> {
        let cx = self.cx;
        let ep = EffectiveParagraph { style: self.default_style.clone(), ..Default::default() };
        let r = cx.sheet.resolve_style_run(&ep);
        r.lang.and_then(|l| l.val.clone()).filter(|v| !v.trim().is_empty())
    }

    /// (distinct revision ids across all stories, any deletion by someone other than the agent).
    fn revision_counts(&self) -> (usize, bool) {
        let cx = self.cx;
        let mut ids = HashSet::new();
        let mut anon = 0;
        let mut foreign = false;
        let mut seen_roots = HashSet::new();
        for st in cx.doc.stories() {
            if !seen_roots.insert((st.part, st.root)) {
                continue;
            }
            let d = cx.doc.part(st.part);
            for rv in view::revisions(d, st.root) {
                match rv.id(d) {
                    Some(id) => {
                        ids.insert(id);
                    }
                    None => anon += 1,
                }
                if matches!(
                    rv.kind,
                    view::RevisionKind::Del
                        | view::RevisionKind::MoveFrom
                        | view::RevisionKind::RowDel
                        | view::RevisionKind::ParaMarkDel
                ) && !cx.is_agent(&rv.author(d).unwrap_or_default())
                {
                    foreign = true;
                }
            }
        }
        (ids.len() + anon, foreign)
    }

    // -------------------------------------------------------------------------- outline

    pub(crate) fn outline_entries(&self) -> Vec<(String, u8, String)> {
        let cx = self.cx;
        let story = cx.story(0);
        let part = story.part;
        let d = self.d(part);
        let mut out = Vec::new();
        let mut stack: Vec<std::vec::IntoIter<Block>> = vec![view::blocks(d, story.root).into_iter()];
        while let Some(it) = stack.last_mut() {
            match it.next() {
                None => {
                    stack.pop();
                }
                Some(Block::Sdt(s)) => stack.push(s.blocks(d).into_iter()),
                Some(Block::Paragraph(p)) => {
                    let Some(level) = self.heading_level(part, p.0) else { continue };
                    let text = cx.accepted_text(part, p);
                    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if text.is_empty() {
                        continue;
                    }
                    let Some(a) = cx.anchors.handle_of(Loc::new(part, p.0)) else { continue };
                    let mut e = format!("[{a}] ");
                    if let Some((l, _)) = self.label_parts(part, p.0) {
                        e.push_str(&l);
                        e.push(' ');
                    }
                    e.push_str(&cut_chars(&text, OUTLINE_ENTRY));
                    out.push((e, level, text));
                }
                Some(Block::Table(_)) => {}
            }
        }
        out
    }

    pub(crate) fn outline_line(&self) -> String {
        let entries = self.outline_entries();
        if entries.is_empty() {
            return "§ outline: (none)".into();
        }
        let full = entries.iter().map(|e| e.0.as_str()).collect::<Vec<_>>().join(" · ");
        if full.chars().count() <= OUTLINE_MAX {
            return format!("§ outline: {full}");
        }
        let mut s = String::from("§ outline: ");
        let mut first = true;
        for (e, level, _) in &entries {
            if *level > 2 {
                continue;
            }
            if s.chars().count() + e.chars().count() + 3 > OUTLINE_MAX {
                break;
            }
            if !first {
                s.push_str(" · ");
            }
            s.push_str(e);
            first = false;
        }
        s.push_str(" · …");
        s
    }
}

// ------------------------------------------------------------------------------- helpers

fn control_entry(c: &Control, anchor: &str) -> String {
    let name = c.name();
    let sep = if name.is_empty() { "" } else { " " };
    let val = match &c.value {
        CtlValue::Text(v) => format!("\"{}\"", inline::quote(&cut_chars(v, ON_TEXT))),
        CtlValue::Empty => "empty".into(),
        CtlValue::Check(on) => (if *on { "☒" } else { "☐" }).into(),
    };
    format!("⟪CTL{sep}{name}⟫ {} in {anchor} = {val}", c.typ)
}

fn rev_tag(cx: &DocCtx, author: &str, id: Option<i64>) -> String {
    match id {
        Some(id) => format!("⟨r{id} {}⟩", cx.author_label(author)),
        None => format!("⟨{}⟩", cx.author_label(author)),
    }
}

/// Remove the revision a shown row change already says from its cell pieces.
fn strip_rev(pieces: &mut Vec<Piece>, kind: RevKind, author: &str) {
    pieces.retain(|p| !matches!(p.atom, Atom::ParaMark));
    for p in pieces.iter_mut() {
        if p.rev.as_ref().is_some_and(|r| r.kind.is_del() == kind.is_del() && r.author == author) {
            p.rev = None;
        }
    }
}

fn image_line(cx: &DocCtx, part: PartIdx, n: NodeId, anchor: &str) -> String {
    let info = image::info(cx, part, n);
    let kind = info.proj_kind();
    let mut s =
        format!("[{anchor} {kind} {} {}", if info.inline { "inline" } else { "anchor" }, image::cm(info.w, info.h));
    if let Some(a) = info.align {
        s.push(' ');
        s.push_str(a);
    }
    if let Some(w) = info.proj_wrap() {
        s.push_str(" wrap=");
        s.push_str(w);
    }
    if let Some(a) = &info.alt {
        s.push_str(&format!(" alt=\"{}\"", inline::quote(a.trim())));
    } else if info.decorative {
        s.push_str(" decorative");
    } else if matches!(kind, "IMAGE" | "CHART" | "DIAGRAM") {
        s.push_str(" alt?");
    }
    if let Some(tb) = cx.textboxes.get(&Loc::new(part, n)) {
        s.push_str(" → ");
        s.push_str(tb);
    }
    s.push(']');
    s
}

fn has_drawing(d: &Document, p: NodeId) -> bool {
    d.descendants(p)
        .any(|n| d.namespace(n) == Some(ns::W) && matches!(d.local_name(n), Some("drawing" | "pict" | "object")))
}

fn tbl_pr(d: &Document, t: NodeId) -> Option<NodeId> {
    d.find_child(t, ns::W, "tblPr")
}

fn row_tr_pr(d: &Document, r: NodeId) -> Option<NodeId> {
    d.find_child(r, ns::W, "trPr")
}

fn grid_cols(d: &Document, t: NodeId) -> Option<usize> {
    let g = d.find_child(t, ns::W, "tblGrid")?;
    let n = d.child_elements(g).filter(|&c| d.is(c, ns::W, "gridCol")).count();
    (n > 0).then_some(n)
}

fn section_line(d: &Document, sp: NodeId, id: usize) -> String {
    let typ = match child_val(d, sp, "type").as_deref() {
        Some("continuous") => "continuous",
        Some("evenPage") => "even page",
        Some("oddPage") => "odd page",
        Some("nextColumn") => "next column",
        _ => "next page",
    };
    let mut s = format!("§ section s{id} · {typ}");
    if d.find_child(sp, ns::W, "pgSz").and_then(|p| w_attr(d, p, "orient")).as_deref() == Some("landscape") {
        s.push_str(" · landscape");
    }
    if let Some(n) = d
        .find_child(sp, ns::W, "cols")
        .and_then(|c| w_attr(d, c, "num"))
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|&n| n > 1)
    {
        s.push_str(&format!(" · {n} columns"));
    }
    s
}

/// The instruction of a TOC field that begins in paragraph `p`.
fn toc_instr(d: &Document, p: NodeId) -> Option<String> {
    let mut instr = String::new();
    // Only the outermost field's own instruction (entries hold nested PAGEREF fields).
    let mut depth = 0i32;
    let mut done = false;
    for n in d.descendants(p) {
        if d.namespace(n) != Some(ns::W) || done {
            continue;
        }
        match d.local_name(n) {
            Some("fldChar") => match w_attr(d, n, "fldCharType").as_deref() {
                Some("begin") => depth += 1,
                Some("separate") if depth == 1 => done = true,
                Some("end") => {
                    depth -= 1;
                    if depth <= 0 {
                        done = !instr.trim().is_empty();
                    }
                }
                _ => {}
            },
            Some("instrText") if depth == 1 => instr.push_str(&d.text_content(n)),
            Some("fldSimple") if instr.trim().is_empty() => {
                if let Some(i) = w_attr(d, n, "instr") {
                    instr.push_str(&i);
                    instr.push(' ');
                }
            }
            _ => {}
        }
    }
    let t = instr.trim_start();
    let kw = t.split_whitespace().next()?;
    kw.eq_ignore_ascii_case("TOC").then(|| t.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Index of the last block of the TOC field that begins at `flat[i]`.
fn toc_end(d: &Document, flat: &[Block], i: usize) -> usize {
    let mut depth: i64 = 0;
    for (k, b) in flat.iter().enumerate().skip(i) {
        let Block::Paragraph(p) = b else { return k.saturating_sub(1).max(i) };
        for n in d.descendants(p.0) {
            if d.is(n, ns::W, "fldChar") {
                match w_attr(d, n, "fldCharType").as_deref() {
                    Some("begin") => depth += 1,
                    Some("end") => depth -= 1,
                    _ => {}
                }
            }
        }
        if depth <= 0 {
            return k;
        }
    }
    flat.len() - 1
}

fn fold_empties(units: Vec<Unit>) -> Vec<Unit> {
    let mut out: Vec<Unit> = Vec::with_capacity(units.len());
    let mut run: Vec<Unit> = Vec::new();
    let flush = |run: &mut Vec<Unit>, out: &mut Vec<Unit>| {
        if run.len() > 2 {
            let first = run[0].head.clone();
            let last = run[run.len() - 1].head.clone();
            let n = run.len();
            out.push(Unit {
                kind: UnitKind::Fold,
                section: run[0].section.take(),
                lines: vec![format!("[{first}..{last}] ({n} empty)")],
                head: first,
                tail: last,
                nodes: run.iter().flat_map(|u| u.nodes.iter().copied()).collect(),
                blocks: run.iter().map(|u| u.blocks).sum(),
                empty: true,
                heading: None,
                rows: Vec::new(),
            });
            run.clear();
        } else {
            out.append(run);
        }
    };
    for u in units {
        if u.empty && u.kind == UnitKind::Para && (run.is_empty() || u.section.is_none()) {
            run.push(u);
        } else {
            flush(&mut run, &mut out);
            if u.empty && u.kind == UnitKind::Para {
                run.push(u);
            } else {
                out.push(u);
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

pub(crate) fn xml_elem_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let start = xml.find(&open)?;
    let rest = &xml[start + open.len()..];
    let gt = rest.find('>')?;
    if rest[..gt].ends_with('/') {
        return None;
    }
    let body = &rest[gt + 1..];
    let end = body.find(&format!("</{tag}>"))?;
    let raw = &body[..end];
    Some(
        raw.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

// ------------------------------------------------------------------------------- doc facts

/// Read a package part as text.
pub(crate) fn part_text(doc: &WordDoc, name: &str) -> Option<String> {
    doc.package().and_then(|p| p.part(name).ok()).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Title: `dc:title`, else the first heading, else the file name without extension.
pub(crate) fn doc_title(cx: &DocCtx, first_heading: Option<String>) -> Option<String> {
    part_text(cx.doc, "docProps/core.xml")
        .as_deref()
        .and_then(|c| xml_elem_text(c, "dc:title"))
        .filter(|t| !t.trim().is_empty())
        .or(first_heading)
        .or_else(|| {
            cx.opts.file_name.as_deref().map(|f| {
                let base = f.rsplit(['/', '\\']).next().unwrap_or(f);
                base.rsplit_once('.').map(|(a, _)| a).unwrap_or(base).to_owned()
            })
        })
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
}

/// Page count: `app.xml` `Pages`, else one page per 1800 characters.
pub(crate) fn doc_pages(cx: &DocCtx, body_chars: usize) -> u64 {
    part_text(cx.doc, "docProps/app.xml")
        .as_deref()
        .and_then(|a| xml_elem_text(a, "Pages"))
        .and_then(|p| p.trim().parse::<u64>().ok())
        .filter(|&p| p > 0)
        .unwrap_or_else(|| (body_chars as u64).div_ceil(1800).max(1))
}

/// Facts about the package itself.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PackageFlags {
    pub macros: bool,
    /// External relationships other than hyperlinks (linked images, templates, OLE links).
    pub external_links: bool,
    pub protected: bool,
    pub signed: bool,
    pub embedded_fonts: bool,
}

pub(crate) fn package_flags(doc: &WordDoc) -> PackageFlags {
    let names: Vec<String> =
        doc.package().map(|p| p.names().map(|n| n.to_ascii_lowercase()).collect()).unwrap_or_default();
    PackageFlags {
        macros: names.iter().any(|n| n.ends_with("vbaproject.bin")),
        external_links: doc
            .part_indices()
            .any(|i| doc.rels(i).iter().any(|r| r.external && !r.rel_type.ends_with("/hyperlink"))),
        protected: ctx::protected(doc),
        signed: names.iter().any(|n| n.starts_with("_xmlsignatures/")),
        embedded_fonts: names.iter().any(|n| n.starts_with("word/fonts/") && n.ends_with(".odttf")),
    }
}

pub(crate) fn cut_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_owned();
    }
    let mut o: String = s.chars().take(n).collect();
    o = o.trim_end().to_owned();
    o.push('…');
    o
}

fn line_tokens(lines: &[String]) -> usize {
    lines.iter().map(|l| estimate_tokens(l) + 1).sum()
}

/// Cut a long block line at a sentence boundary so it fits `tokens`.
fn cut_line(line: &str, tokens: usize, anchor: &str) -> String {
    let max_chars = (tokens * 36 / 10).saturating_sub(30).max(80);
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    if chars.len() <= max_chars {
        return line.to_owned();
    }
    let limit = chars[max_chars].0;
    let head = &line[..limit];
    let cut = head
        .rmatch_indices(['.', '?', '!', ';'])
        .map(|(i, _)| i + 1)
        .find(|&i| line[i..].starts_with(' ') && i > limit / 3)
        .or_else(|| head.rfind(' '))
        .unwrap_or(limit);
    format!("{} …⟨more: read {anchor}⟩", line[..cut].trim_end())
}

/// First sentence of a block line (outline mode).
fn first_sentence(line: &str) -> String {
    let close = line.find("] ").map(|i| i + 2).unwrap_or(0);
    let rest = &line[close..];
    let end = rest
        .match_indices(['.', '?', '!'])
        .map(|(i, _)| i + 1)
        .find(|&i| i >= rest.len() || rest[i..].starts_with(' '));
    match end {
        Some(e) if e < rest.len() => format!("{}{} …", &line[..close], &rest[..e]),
        _ => line.to_owned(),
    }
}

// ------------------------------------------------------------------------------- read

struct Story0 {
    units: Vec<Unit>,
    index: HashMap<NodeId, usize>,
}

fn index_units(units: Vec<Unit>) -> Story0 {
    let mut index = HashMap::new();
    for (i, u) in units.iter().enumerate() {
        for &n in &u.nodes {
            index.insert(n, i);
        }
    }
    Story0 { units, index }
}

/// Index of the story whose tree holds `loc` (the innermost one).
fn story_of(doc: &WordDoc, loc: Loc) -> Option<usize> {
    let d = doc.part(loc.part);
    let mut best = None;
    for (si, st) in doc.stories().iter().enumerate() {
        if st.part != loc.part {
            continue;
        }
        if st.root == loc.node || d.ancestors(loc.node).any(|a| a == st.root) {
            best = Some(si);
        }
    }
    best
}

/// Node an anchor stands for (paragraph, table, drawing; rows and cells map to the table).
fn target_loc(t: Target) -> Option<Loc> {
    Some(match t {
        Target::Block { para, .. } => para,
        Target::Table { table } | Target::Row { table, .. } | Target::Cell { table, .. } => table,
        Target::Image { drawing } => drawing,
        Target::Comment { .. } => return None,
    })
}

/// Unit index holding `loc` within a story's units.
fn unit_of(doc: &WordDoc, s: &Story0, loc: Loc) -> Option<usize> {
    let d = doc.part(loc.part);
    if let Some(&i) = s.index.get(&loc.node) {
        return Some(i);
    }
    d.ancestors(loc.node).find_map(|a| s.index.get(&a).copied())
}

pub(crate) fn read(
    doc: &WordDoc,
    anchors: &Anchors,
    sheet: &StyleSheet,
    idx: Option<&DocIndex>,
    req: &ReadRequest,
) -> Result<ReadResult> {
    let cx = DocCtx::with(doc, anchors, sheet, &req.options, req.view, idx);
    let budget = req.budget.unwrap_or(DEFAULT_BUDGET).max(MIN_BUDGET);
    let mut r = Renderer::new(&cx);
    let (text, next) = match (&req.range, req.mode) {
        (None, ReadMode::Full) => {
            let f = r.first_window(budget);
            (f.text, f.next)
        }
        (_, ReadMode::Outline) => (r.outline_mode(budget), None),
        (Some(range), ReadMode::Full) => r.range_read(range, budget, req.stories)?,
    };
    let tokens = estimate_tokens(&text);
    Ok(ReadResult { text, next, tokens })
}

struct FirstWindow {
    text: String,
    next: Option<String>,
}

impl Renderer<'_, '_> {
    fn first_window(&mut self, budget: usize) -> FirstWindow {
        self.collecting = true;
        let body = index_units(self.story_units(0));
        let stories = self.stories();
        let appendix = self.appendix();
        self.collecting = false;
        let body_chars: usize = {
            let d = self.d(self.cx.story(0).part);
            view::paragraph_texts(d, self.cx.story(0).root, crate::wml::TextMode::AcceptAll)
                .iter()
                .map(|t| t.chars().count())
                .sum()
        };
        let first_heading = self.outline_entries().into_iter().next().map(|e| e.2);
        let header = self.header(body_chars, first_heading);
        let outline = self.outline_line();
        let fixed = estimate_tokens(&header)
            + estimate_tokens(&outline)
            + 20
            + if stories.is_empty() { 0 } else { line_tokens(&stories) + 1 }
            + if appendix.is_empty() { 0 } else { line_tokens(&appendix) + 1 };
        let window_budget = budget.saturating_sub(fixed).max(budget / 4);
        let total: usize = body.units.iter().map(|u| u.blocks).sum();
        let mut lines = Vec::new();
        let taken = take_units(&body.units, 0, None, body.units.len().saturating_sub(1), window_budget, &mut lines);
        let window = if body.units.is_empty() {
            "§ window (empty)".to_owned()
        } else {
            let last = taken.last_unit;
            let blocks: usize = body.units[..=last].iter().map(|u| u.blocks).sum();
            format!("§ window {}..{} (blocks 1–{} of {})", body.units[0].head, body.units[last].tail, blocks, total)
        };
        let mut out = vec![header, outline, window, String::new()];
        out.extend(lines);
        if !stories.is_empty() {
            out.push(String::new());
            out.extend(stories);
        }
        if !appendix.is_empty() {
            out.push(String::new());
            out.extend(appendix);
        }
        let mut text = out.join("\n");
        text.push('\n');
        FirstWindow { text, next: taken.next.map(|n| format!("{n}..")) }
    }

    fn outline_mode(&mut self, budget: usize) -> String {
        let body = self.story_units(0);
        let body_chars: usize = body.iter().flat_map(|u| u.lines.iter()).map(|l| l.chars().count()).sum();
        let first_heading = self.outline_entries().into_iter().next().map(|e| e.2);
        let header = self.header(body_chars, first_heading);
        let outline = self.outline_line();
        let mut used = estimate_tokens(&header) + estimate_tokens(&outline) + 2;
        let mut out = vec![header, outline, String::new()];
        let mut i = 0;
        while i < body.len() {
            let u = &body[i];
            if u.heading.is_some() {
                let mut add = u.all_lines();
                if let Some(n) = body.get(i + 1)
                    && n.kind == UnitKind::Para
                    && n.heading.is_none()
                    && !n.empty
                {
                    add.push(first_sentence(&n.lines[0]));
                }
                let cost = line_tokens(&add);
                if used + cost > budget {
                    out.push("§ … (outline cut by the budget)".into());
                    break;
                }
                used += cost;
                out.extend(add);
            }
            i += 1;
        }
        let mut text = out.join("\n");
        text.push('\n');
        text
    }

    fn range_read(&mut self, range: &str, budget: usize, stories: bool) -> Result<(String, Option<String>)> {
        let doc = self.cx.doc;
        let parts = select::parse(range)?;
        let mut out: Vec<String> = Vec::new();
        let mut used = 0usize;
        let mut next: Option<String> = None;
        for (pi, part) in parts.iter().enumerate() {
            let remaining = budget.saturating_sub(used);
            if pi > 0 && remaining < budget / 20 {
                next = Some(parts[pi..].iter().map(Sel::text).collect::<Vec<_>>().join(", "));
                break;
            }
            let before = out.len();
            match part {
                Sel::One(a) => self.read_one(a, &mut out)?,
                Sel::Range(a, b) => {
                    // Story of the range.
                    let loc_a = match a {
                        Some(a) => Some(self.anchor_loc(a)?),
                        None => None,
                    };
                    let loc_b = match b {
                        Some(b) => Some(self.anchor_loc(b)?),
                        None => None,
                    };
                    let si = match (loc_a, loc_b) {
                        (Some(l), _) | (None, Some(l)) => story_of(doc, l).unwrap_or(0),
                        _ => 0,
                    };
                    if let (Some(la), Some(lb)) = (loc_a, loc_b)
                        && story_of(doc, la) != story_of(doc, lb)
                    {
                        return Err(Error::Anchor(format!("range {} spans two stories", part.text())));
                    }
                    // Render only the window: from the range start until the range end or the
                    // budget (one unit past it, so the window knows where the next one starts).
                    let story = self.cx.story(si);
                    let flat = self.flat_blocks(story.part, story.root);
                    if flat.is_empty() {
                        continue;
                    }
                    let d = self.d(story.part);
                    let pos: HashMap<NodeId, usize> = flat.iter().enumerate().map(|(k, b)| (b.node(), k)).collect();
                    let top = |l: Loc| {
                        pos.get(&l.node).copied().or_else(|| d.ancestors(l.node).find_map(|x| pos.get(&x).copied()))
                    };
                    let not_block = |x: &Option<String>| {
                        Error::Anchor(format!("{} is not a block of its story", x.as_deref().unwrap_or("")))
                    };
                    let from_flat = match loc_a {
                        Some(l) => top(l).ok_or_else(|| not_block(a))?,
                        None => 0,
                    };
                    let to_flat = match loc_b {
                        Some(l) => Some(top(l).ok_or_else(|| not_block(b))?),
                        None => None,
                    };
                    if to_flat.is_some_and(|t| t < from_flat) {
                        return Err(Error::Anchor(format!("range {} ends before it starts", part.text())));
                    }
                    // A range starting on a row or cell starts at that row.
                    let from_row = a.as_deref().and_then(row_no);
                    let (start, st, pending) = self.window_start(si, &flat, from_flat);
                    let (mut seen, mut cost, mut over_at) = (0usize, 0usize, None::<usize>);
                    let units = self.units_of(si, &flat, start, st, pending, |i, units| {
                        if to_flat.is_some_and(|t| i > t) {
                            return true;
                        }
                        if let Some(n) = over_at {
                            return units.len() > n;
                        }
                        for u in &units[seen..] {
                            let k = u.nodes.first().and_then(|n| pos.get(n)).copied().unwrap_or(0);
                            // The first unit of a row range costs less than all its rows.
                            if u.empty || k < from_flat || (k == from_flat && from_row.is_some()) {
                                continue;
                            }
                            cost += line_tokens(&u.all_lines());
                        }
                        seen = units.len();
                        if cost > remaining {
                            over_at = Some(units.len());
                        }
                        false
                    });
                    let s = index_units(units);
                    if s.units.is_empty() {
                        continue;
                    }
                    let first_after = || {
                        s.units
                            .iter()
                            .position(|u| u.nodes.first().and_then(|n| pos.get(n)).is_some_and(|&k| k >= from_flat))
                    };
                    let from = match loc_a {
                        Some(l) => match unit_of(doc, &s, l).or_else(first_after) {
                            Some(f) => f,
                            None => continue,
                        },
                        None => 0,
                    };
                    let to = match loc_b {
                        Some(l) => unit_of(doc, &s, l).unwrap_or(s.units.len() - 1),
                        None => s.units.len() - 1,
                    };
                    if to < from {
                        return Err(Error::Anchor(format!("range {} ends before it starts", part.text())));
                    }
                    let taken = take_units(&s.units, from, from_row, to, remaining, &mut out);
                    if let Some(n) = taken.next {
                        let end = b.clone().unwrap_or_default();
                        let mut rest = vec![format!("{n}..{end}")];
                        rest.extend(parts[pi + 1..].iter().map(Sel::text));
                        next = Some(rest.join(", "));
                        break;
                    }
                }
            }
            used += line_tokens(&out[before..]);
        }
        if stories {
            let s = self.stories();
            if !s.is_empty() {
                out.push(String::new());
                out.extend(s);
            }
        }
        Ok((out.join("\n"), next))
    }

    fn anchor_loc(&self, a: &str) -> Result<Loc> {
        let t = self.cx.anchors.resolve(self.cx.doc, a).ok_or_else(|| Error::Anchor(format!("unknown anchor {a}")))?;
        target_loc(t).ok_or_else(|| Error::Anchor(format!("{a} can't be part of a range")))
    }

    /// One anchor or story id, whole.
    fn read_one(&mut self, a: &str, out: &mut Vec<String>) -> Result<()> {
        let cx = self.cx;
        let doc = cx.doc;
        if anchors::is_comment_id(a) {
            let idx = cx
                .comments
                .iter()
                .position(|c| c.sid == a)
                .ok_or_else(|| Error::Anchor(format!("unknown comment {a}")))?;
            // The quoted range needs a body pass.
            self.collecting = true;
            for si in 0..doc.stories().len() {
                if matches!(cx.story(si).kind, StoryKind::Body | StoryKind::Header { .. } | StoryKind::Footer { .. }) {
                    let _ = self.story_units(si);
                }
            }
            self.collecting = false;
            out.extend(self.comment_lines(idx));
            return Ok(());
        }
        if !anchors::is_anchor(a) && anchors::is_story_id(a) {
            let si = cx.anchors.story_index(a).ok_or_else(|| Error::Anchor(format!("unknown story {a}")))?;
            if si == 0 {
                let units = self.story_units(0);
                for u in units {
                    out.extend(u.all_lines());
                }
            } else if let Some(s) = self.story_section(si) {
                out.extend(s);
            } else {
                out.push(self.story_line(si));
            }
            return Ok(());
        }
        let t = cx.anchors.resolve(doc, a).ok_or_else(|| Error::Anchor(format!("unknown anchor {a}")))?;
        self.target_lines(t, out);
        Ok(())
    }

    fn target_lines(&mut self, t: Target, out: &mut Vec<String>) {
        let cx = self.cx;
        let doc = cx.doc;
        match t {
            Target::Block { para, .. } => {
                let d = doc.part(para.part);
                let in_table = Paragraph(para.node).cell(d).is_some();
                if let Some(instr) = toc_instr(d, para.node).filter(|_| !in_table) {
                    let _ = instr;
                    // A TOC renders as its group line.
                    if let Some(si) = story_of(doc, para) {
                        let story = cx.story(si);
                        let flat = self.flat_blocks(story.part, story.root);
                        if let Some(k) = flat.iter().position(|b| b.node() == para.node) {
                            let (start, st, pending) = self.window_start(si, &flat, k);
                            let units = self.units_of(si, &flat, start, st, pending, |i, u| i > k && !u.is_empty());
                            let s = index_units(units);
                            if let Some(u) = unit_of(doc, &s, para).map(|i| &s.units[i]) {
                                out.extend(u.lines.iter().cloned());
                                return;
                            }
                        }
                    }
                }
                let mut st = InlineState::default();
                match self.para(para.part, para.node, &mut st, in_table) {
                    Some(o) => out.push(o.line),
                    None => {
                        let a = cx.anchors.handle_of(para).unwrap_or("?");
                        out.push(format!("[{a}]"));
                    }
                }
            }
            Target::Table { table } => {
                let mut st = InlineState::default();
                let u = self.table(table.part, Table(table.node), &mut st);
                out.extend(u.lines);
            }
            Target::Row { table, row } => {
                let mut st = InlineState::default();
                let u = self.table(table.part, Table(table.node), &mut st);
                let d = doc.part(table.part);
                let rno = Table(table.node).rows(d).iter().position(|r| r.0 == row.node).map(|i| i + 1);
                out.push(u.lines[0].clone());
                if let Some(rno) = rno {
                    out.extend(table_rows(&u, rno, Some(rno)));
                }
            }
            Target::Cell { cell, .. } => {
                let d = doc.part(cell.part);
                let blocks = view::Cell(cell.node).blocks(d);
                let mut st = InlineState::default();
                self.blocks_into(cell.part, &blocks, &mut st, true, out);
            }
            Target::Image { drawing } => {
                let d = doc.part(drawing.part);
                if let Some(p) = d.ancestors(drawing.node).find(|&a| d.is(a, ns::W, "p")) {
                    let in_table = Paragraph(p).cell(d).is_some();
                    let mut st = InlineState::default();
                    if let Some(o) = self.para(drawing.part, p, &mut st, in_table) {
                        out.push(o.line);
                    }
                }
            }
            Target::Comment { .. } => {}
        }
    }

    pub(crate) fn block_lines(&mut self, handles: &[&str]) -> Vec<String> {
        let mut out = Vec::new();
        for h in handles {
            if anchors::is_comment_id(h) {
                let _ = self.read_one(h, &mut out);
                continue;
            }
            if let Some(t) = self.cx.anchors.resolve(self.cx.doc, h) {
                self.target_lines(t, &mut out);
            }
        }
        out
    }
}

/// `t1.r5` / `t1.r5.c2` → 5.
fn row_no(a: &str) -> Option<usize> {
    if !(anchors::is_row_anchor(a) || anchors::is_cell_anchor(a)) {
        return None;
    }
    let r = a.split('.').nth(1)?;
    r.strip_prefix('r')?.parse().ok()
}

/// The lines of rows `from..=to` (1-based row numbers) of a table unit.
fn table_rows(u: &Unit, from: usize, to: Option<usize>) -> Vec<String> {
    let mut out = Vec::new();
    for (k, &(li, rno)) in u.rows.iter().enumerate() {
        if rno < from || to.is_some_and(|t| rno > t) {
            continue;
        }
        let end = u.rows.get(k + 1).map(|r| r.0).unwrap_or(u.lines.len());
        out.extend(u.lines[li..end].iter().cloned());
    }
    out
}

struct Taken {
    /// Index of the last unit (partly) printed.
    last_unit: usize,
    /// Anchor to continue from.
    next: Option<String>,
}

/// Print units `from..=to` into `out` within `budget` tokens.
fn take_units(
    units: &[Unit],
    from: usize,
    from_row: Option<usize>,
    to: usize,
    budget: usize,
    out: &mut Vec<String>,
) -> Taken {
    let mut used = 0;
    let mut i = from;
    let mut last_unit = from;
    while i <= to && i < units.len() {
        let u = &units[i];
        let lines = match (i == from, from_row) {
            (true, Some(r)) if u.kind == UnitKind::Table => {
                let mut v = vec![u.lines[0].clone()];
                v.extend(table_rows(u, r, None));
                v
            }
            _ => u.all_lines(),
        };
        let cost = line_tokens(&lines);
        if used + cost > budget {
            if i > from {
                return Taken { last_unit, next: Some(u.head.clone()) };
            }
            // The first unit alone is over the budget.
            last_unit = i;
            if u.kind == UnitKind::Table && u.rows.len() > 1 {
                let start_row = from_row.unwrap_or(1);
                let mut v = Vec::new();
                if let Some(s) = &u.section {
                    v.push(s.clone());
                }
                v.push(u.lines[0].clone());
                let mut cost = line_tokens(&v);
                for (k, &(li, rno)) in u.rows.iter().enumerate() {
                    if rno < start_row {
                        continue;
                    }
                    let end = u.rows.get(k + 1).map(|r| r.0).unwrap_or(u.lines.len());
                    let rl = &u.lines[li..end];
                    let c = line_tokens(rl);
                    if cost + c > budget && v.len() > 1 + usize::from(u.section.is_some()) {
                        out.extend(v);
                        return Taken { last_unit, next: Some(format!("{}.r{rno}", u.head)) };
                    }
                    cost += c;
                    v.extend(rl.iter().cloned());
                }
                out.extend(v);
            } else {
                for l in lines {
                    if estimate_tokens(&l) > budget {
                        out.push(cut_line(&l, budget, &u.head));
                    } else {
                        out.push(l);
                    }
                }
            }
            i += 1;
            return Taken { last_unit, next: (i <= to && i < units.len()).then(|| units[i].head.clone()) };
        }
        used += cost;
        out.extend(lines);
        last_unit = i;
        i += 1;
    }
    Taken { last_unit, next: None }
}

// ------------------------------------------------------------------------------- entry points

pub(crate) fn block_lines(
    doc: &WordDoc,
    anchors: &Anchors,
    sheet: &StyleSheet,
    idx: Option<&DocIndex>,
    handles: &[&str],
    view: View,
    options: &ProjectOptions,
) -> Result<Vec<String>> {
    let cx = DocCtx::with(doc, anchors, sheet, options, view, idx);
    let mut r = Renderer::new(&cx);
    Ok(r.block_lines(handles))
}

/// Every content control, placeholder and cross-reference in the body, headers, footers,
/// notes and text boxes (not comments), in the accepted view, each once per anchor.
pub(crate) fn fields(
    doc: &WordDoc,
    anchors: &Anchors,
    sheet: &StyleSheet,
    idx: Option<&DocIndex>,
    options: &ProjectOptions,
) -> Result<Fields> {
    let cx = DocCtx::with(doc, anchors, sheet, options, View::Accepted, idx);
    let mut r = Renderer::new(&cx);
    r.collecting = true;
    for si in 0..doc.stories().len() {
        if !matches!(cx.story(si).kind, StoryKind::Comment { .. }) {
            let _ = r.story_units(si);
        }
    }
    let mut f = std::mem::take(&mut r.col.found);
    let mut seen = HashSet::new();
    f.controls.retain(|c| seen.insert(format!("{:?}", c)));
    let mut seen = HashSet::new();
    f.placeholders.retain(|p| seen.insert((p.anchor.clone(), p.text.clone())));
    let mut seen = HashSet::new();
    f.refs.retain(|x| seen.insert((x.anchor.clone(), x.typ.clone(), x.bookmark.clone())));
    Ok(f)
}

/// [`fields`] of the given blocks only.
pub(crate) fn fields_for(
    doc: &WordDoc,
    anchors: &Anchors,
    sheet: &StyleSheet,
    idx: Option<&DocIndex>,
    options: &ProjectOptions,
    handles: &[&str],
) -> Result<Fields> {
    let cx = DocCtx::with(doc, anchors, sheet, options, View::Accepted, idx);
    let mut r = Renderer::new(&cx);
    r.collecting = true;
    let _ = r.block_lines(handles);
    Ok(std::mem::take(&mut r.col.found))
}

/// The heading level of each body paragraph outside tables, in document order, up to and
/// including `until`.
pub(crate) fn body_headings(
    doc: &WordDoc,
    anchors: &Anchors,
    sheet: &StyleSheet,
    idx: Option<&DocIndex>,
    options: &ProjectOptions,
    until: &str,
) -> Vec<(String, Option<u8>)> {
    let cx = DocCtx::with(doc, anchors, sheet, options, View::Accepted, idx);
    let r = Renderer::new(&cx);
    let Some(body) = doc.stories().iter().position(|s| matches!(s.kind, StoryKind::Body)) else { return Vec::new() };
    let mut out = Vec::new();
    for (h, loc) in anchors.handles(body) {
        let d = doc.part(loc.part);
        if !d.ancestors(loc.node).any(|a| crate::wml::tree::w_local(d, a) == Some("tc")) {
            out.push((h.to_owned(), r.heading_level(loc.part, loc.node)));
        }
        if h == until {
            break;
        }
    }
    out
}

pub(crate) fn outline_line(
    doc: &WordDoc,
    anchors: &Anchors,
    sheet: &StyleSheet,
    idx: Option<&DocIndex>,
    options: &ProjectOptions,
) -> Result<String> {
    let cx = DocCtx::with(doc, anchors, sheet, options, View::Accepted, idx);
    let r = Renderer::new(&cx);
    Ok(r.outline_line())
}
