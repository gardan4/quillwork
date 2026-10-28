//! The inline content of one paragraph as a flat list of [`Piece`]s (text, tabs, fields,
//! controls, images, note and comment references, the paragraph mark), each with the formatting,
//! revision, link and untrusted-content fence it renders with; and the projection rendering of
//! such a list (spec/projection.md §4–§5).

use std::collections::HashMap;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::wml::style::{EffectiveRun, Script};
use crate::wml::text::{TextMode, text};
use crate::wml::tree::{child_on, child_val, is_ignored_branch, is_props, w_attr};
use crate::wml::view::{Paragraph, Revision, RevisionKind, Run};
use crate::wml::{Loc, PartIdx};
use crate::xml::{Document, NodeId, ns};

use super::ctx::{DocCtx, delta_e, highlight_rgb};

// ---------------------------------------------------------------------------------- data

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrKind {
    Line,
    Page,
    Column,
}

impl BrKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BrKind::Line => "line",
            BrKind::Page => "page",
            BrKind::Column => "column",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CtlValue {
    Text(String),
    Empty,
    Check(bool),
}

/// A content control (`w:sdt`), inline or around blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Control {
    pub tag: Option<String>,
    pub alias: Option<String>,
    /// View-model `ContentControl.type`.
    pub typ: &'static str,
    pub locked: bool,
    pub bound: bool,
    pub value: CtlValue,
    /// What the control shows: its content's accepted text (placeholder text included), or
    /// `☒` / `☐` for a checkbox.
    pub display: String,
    /// The choices of a dropdown or combo box (`w:listItem` display text, else its value).
    pub items: Vec<String>,
}

impl Control {
    /// Read the properties of a `w:sdt`.
    pub fn of(d: &Document, sdt: NodeId) -> Control {
        let pr = d.find_child(sdt, ns::W, "sdtPr");
        let content = d.find_child(sdt, ns::W, "sdtContent");
        let mut typ = "richText";
        let mut check = None;
        let (mut locked, mut bound, mut placeholder) = (false, false, false);
        let mut tag = None;
        let mut alias = None;
        let mut items = Vec::new();
        if let Some(pr) = pr {
            for c in d.child_elements(pr) {
                let local = d.local_name(c).unwrap_or("");
                if d.namespace(c) == Some(ns::W14) && local == "checkbox" {
                    typ = "checkbox";
                    check = Some(
                        d.child_elements(c)
                            .find(|&x| d.local_name(x) == Some("checked"))
                            .and_then(|x| d.attr_ns(x, ns::W14, "val"))
                            .is_some_and(|v| matches!(v.as_ref(), "1" | "true")),
                    );
                    continue;
                }
                if d.namespace(c) != Some(ns::W) {
                    continue;
                }
                match local {
                    "tag" => tag = w_attr(d, c, "val").filter(|s| !s.is_empty()),
                    "alias" => alias = w_attr(d, c, "val").filter(|s| !s.is_empty()),
                    "date" => typ = "date",
                    "dropDownList" | "comboBox" => {
                        typ = if local == "comboBox" { "comboBox" } else { "dropdown" };
                        for it in d.child_elements(c).filter(|&x| d.is(x, ns::W, "listItem")) {
                            let label = w_attr(d, it, "displayText")
                                .filter(|s| !s.is_empty())
                                .or_else(|| w_attr(d, it, "value"))
                                .unwrap_or_default();
                            items.push(label);
                        }
                    }
                    "picture" => typ = "picture",
                    "text" => typ = "plainText",
                    "group" => typ = "group",
                    "docPartObj" | "docPartList" | "citation" | "bibliography" | "equation" => typ = "other",
                    "showingPlcHdr" => placeholder = !matches!(w_attr(d, c, "val").as_deref(), Some("0" | "false")),
                    "dataBinding" => bound = true,
                    "lock" => {
                        locked = matches!(w_attr(d, c, "val").as_deref(), Some("contentLocked" | "sdtContentLocked"))
                    }
                    _ => {}
                }
            }
        }
        let t = content.map(|c| text(d, c, TextMode::AcceptAll)).unwrap_or_default();
        let (value, display) = match check {
            Some(on) => (CtlValue::Check(on), if on { "☒" } else { "☐" }.to_owned()),
            None if placeholder || t.trim().is_empty() => (CtlValue::Empty, t),
            None => (CtlValue::Text(t.clone()), t),
        };
        Control { tag, alias, typ, locked, bound, value, display, items }
    }

    /// `tag="x"` (or `alias="x"`), for the projection.
    pub fn name(&self) -> String {
        match (&self.tag, &self.alias) {
            (Some(t), _) => format!("tag=\"{}\"", quote(t)),
            (None, Some(a)) => format!("alias=\"{}\"", quote(a)),
            _ => String::new(),
        }
    }

    pub fn render(&self) -> String {
        let name = self.name();
        let sep = if name.is_empty() { "" } else { " " };
        match &self.value {
            CtlValue::Text(v) => format!("⟪CTL{sep}{name} → \"{}\"⟫", quote(v)),
            CtlValue::Empty => format!("⟪CTL{sep}{name} empty⟫"),
            CtlValue::Check(on) => format!("⟪CTL{sep}{name} → {}⟫", if *on { "☒" } else { "☐" }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldAtom {
    pub instr: String,
    pub result: String,
    pub dirty: bool,
    /// HYPERLINK target.
    pub link: Option<Link>,
}

impl FieldAtom {
    /// Instruction keyword, upper case.
    pub fn typ(&self) -> String {
        self.instr.split_whitespace().next().unwrap_or("").to_ascii_uppercase()
    }

    /// Arguments: everything after the keyword, whitespace collapsed.
    pub fn args(&self) -> String {
        self.instr.split_whitespace().skip(1).collect::<Vec<_>>().join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Atom {
    Text(String),
    Tab,
    Br(BrKind),
    Field(Box<FieldAtom>),
    Control(Box<Control>),
    /// A drawing (`w:drawing`, `w:pict`, `w:object`) in the paragraph's part.
    Image(NodeId),
    Note {
        endnote: bool,
        id: i64,
    },
    /// A comment marker (`w:id`).
    Comment(i64),
    ParaMark,
    /// A symbol-font character (`w:sym` in the private use area).
    Sym(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RevKind {
    Ins,
    Del,
    MoveFrom,
    MoveTo,
}

impl RevKind {
    pub fn is_del(self) -> bool {
        matches!(self, RevKind::Del | RevKind::MoveFrom)
    }
    pub fn vm(self) -> &'static str {
        match self {
            RevKind::Ins => "ins",
            RevKind::Del => "del",
            RevKind::MoveFrom => "moveFrom",
            RevKind::MoveTo => "moveTo",
        }
    }
}

/// An insertion, deletion or move around content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rev {
    pub id: Option<i64>,
    pub kind: RevKind,
    pub author: String,
    pub date: Option<String>,
    /// For a move destination: the anchor of the source paragraph.
    pub moved_from: Option<String>,
}

/// A tracked formatting change (`w:rPrChange`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FmtChange {
    pub id: Option<i64>,
    pub author: String,
    pub date: Option<String>,
    /// `+bold -italic`, `font`, … (projection notation).
    pub desc: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Link {
    Url(String),
    Bookmark(String),
}

/// Run formatting that differs from the paragraph's resolved style (view-model `Marks`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Marks {
    /// Absent inherits paragraph formatting; false explicitly disables it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub i: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub u: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub strike: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub dstrike: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub sup: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub sub: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caps: Option<bool>,
    #[serde(skip_serializing_if = "is_false")]
    pub small_caps: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    /// `#RRGGBB`, or `auto` to reset an inherited custom color.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlight: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub rtl: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Marks {
    pub fn is_empty(&self) -> bool {
        *self == Marks::default()
    }

    /// Resolved run formatting. Font, size, color and b/i/caps inherit the paragraph
    /// baseline; decorations and highlighting are emitted in full
    /// because the paragraph view model does not carry those run properties.
    pub fn relative(run: &EffectiveRun, base: &EffectiveRun) -> Marks {
        let s = run.script.unwrap_or(Script::Ascii);
        let on = |v: Option<bool>| v.unwrap_or(false);
        let ul = |r: &EffectiveRun| r.underline.as_ref().and_then(|u| u.val.clone()).filter(|v| v != "none");
        let va = |r: &EffectiveRun| r.vert_align.clone().unwrap_or_default();
        let color = |r: &EffectiveRun| {
            r.color.as_deref().and_then(|c| {
                if c == "auto" {
                    Some("auto".to_owned())
                } else if c.len() == 6 && c.bytes().all(|b| b.is_ascii_hexdigit()) {
                    Some(format!("#{}", c.to_ascii_uppercase()))
                } else {
                    None
                }
            })
        };
        let hl = |r: &EffectiveRun| r.highlight.clone().filter(|h| h != "none");
        let lang = |r: &EffectiveRun| r.lang.as_ref().and_then(|l| l.val.clone());
        let u = ul(run);
        Marks {
            b: (run.bold_for(s) != base.bold_for(s)).then_some(run.bold_for(s)),
            i: (run.italic_for(s) != base.italic_for(s)).then_some(run.italic_for(s)),
            u,
            strike: on(run.strike),
            dstrike: on(run.dstrike),
            sup: va(run) == "superscript",
            sub: va(run) == "subscript",
            caps: (on(run.caps) != on(base.caps)).then_some(on(run.caps)),
            small_caps: on(run.small_caps),
            font: run.font_for(s).filter(|f| Some(*f) != base.font_for(s)).map(str::to_owned),
            size: run.size_for(s).filter(|z| Some(*z) != base.size_for(s) && *z >= 1),
            color: color(run).filter(|c| Some(c) != color(base).as_ref()),
            highlight: hl(run),
            rtl: on(run.rtl) && !on(base.rtl),
            lang: lang(run).filter(|l| Some(l) != lang(base).as_ref()),
        }
    }
}

/// One inline item with everything it renders with.
#[derive(Debug, Clone)]
pub(crate) struct Piece {
    pub atom: Atom,
    /// What this piece contributes to the accepted-mode plain text (`TextMode::AcceptAll`).
    pub plain: String,
    pub marks: Rc<Marks>,
    /// `w:rStyle`.
    pub style: Option<Rc<str>>,
    /// Shown revision (`None` when there is none or the view applies it silently).
    pub rev: Option<Rc<Rev>>,
    pub fmt: Option<Rc<FmtChange>>,
    pub link: Option<Rc<Link>>,
    /// `hidden`, `white`, `tiny`.
    pub fence: Option<&'static str>,
    /// Zero-width characters inside.
    pub zw: bool,
    /// Top-level comment ids (`w:id`) whose range covers this piece.
    pub comments: Rc<[i64]>,
}

// ------------------------------------------------------------------------------ building

#[derive(Clone, Default)]
struct ICtx {
    rev: Option<Rc<Rev>>,
    link: Option<Rc<Link>>,
    /// Inside a deletion (shown or not): contributes no accepted text.
    deleted: bool,
}

#[derive(Debug)]
enum RawKind {
    Atom(Atom),
    Instr(String),
    Begin,
    Sep,
    End,
}

struct Raw {
    kind: RawKind,
    run: Option<NodeId>,
    rev: Option<Rc<Rev>>,
    link: Option<Rc<Link>>,
    deleted: bool,
    comments: Rc<[i64]>,
}

struct RunInfo {
    marks: Rc<Marks>,
    style: Option<Rc<str>>,
    /// Fence for visible text in this run.
    fence: Option<&'static str>,
    fmt: Option<Rc<FmtChange>>,
}

/// Comment ranges open while walking a story (they span paragraphs).
#[derive(Default)]
pub(crate) struct InlineState {
    active: Vec<i64>,
    snapshot: Option<Rc<[i64]>>,
}

impl InlineState {
    /// Advance over `node`'s subtree without rendering it: the comment ranges it opens and
    /// closes (a window that starts mid-story).
    pub(crate) fn skip(&mut self, cx: &DocCtx, d: &Document, node: NodeId) {
        for x in d.descendants(node) {
            if d.namespace(x) != Some(ns::W) {
                continue;
            }
            match d.local_name(x) {
                Some("commentRangeStart") => {
                    if let Some(id) = comment_id(d, x)
                        && cx.comment(id).is_some_and(|ci| ci.parent.is_none())
                        && !self.active.contains(&id)
                    {
                        self.active.push(id);
                        self.snapshot = None;
                    }
                }
                Some("commentRangeEnd") => {
                    if let Some(id) = comment_id(d, x)
                        && let Some(pos) = self.active.iter().position(|&a| a == id)
                    {
                        self.active.remove(pos);
                        self.snapshot = None;
                    }
                }
                _ => {}
            }
        }
    }

    fn current(&mut self) -> Rc<[i64]> {
        if self.snapshot.is_none() {
            self.snapshot = Some(self.active.clone().into());
        }
        self.snapshot.clone().unwrap_or_else(|| Rc::from(Vec::new()))
    }
}

struct Builder<'c, 'a> {
    cx: &'c DocCtx<'a>,
    part: PartIdx,
    d: &'a Document,
    raw: Vec<Raw>,
    st: &'c mut InlineState,
}

/// The pieces of paragraph `p` in `part`, in document order, for the context's view.
pub(crate) fn pieces(cx: &DocCtx, part: PartIdx, p: NodeId, st: &mut InlineState) -> Vec<Piece> {
    let d = cx.doc.part(part);
    let mut b = Builder { cx, part, d, raw: Vec::new(), st };
    b.container(p, &ICtx::default());
    let raw = std::mem::take(&mut b.raw);
    let mut out = fold(cx, part, p, raw);
    // The paragraph mark.
    if let Some(rv) = Paragraph(p).mark_revision(d) {
        let kind = match rv.kind {
            RevisionKind::ParaMarkIns => Some(RevKind::Ins),
            RevisionKind::ParaMarkDel => Some(RevKind::Del),
            _ => None,
        };
        if let Some(kind) = kind {
            let author = rv.author(d).unwrap_or_default();
            let id = rv.id(d);
            if cx.shows(&author, id) {
                out.push(Piece {
                    atom: Atom::ParaMark,
                    plain: String::new(),
                    marks: Rc::default(),
                    style: None,
                    rev: Some(Rc::new(Rev { id, kind, author, date: rv.date(d), moved_from: None })),
                    fmt: None,
                    link: None,
                    fence: None,
                    zw: false,
                    comments: Rc::from(Vec::new()),
                });
            }
        }
    }
    merge(out)
}

/// The paragraph-mark revision of `p` in view-model form, whatever the view.
pub(crate) fn mark_rev(d: &Document, p: NodeId) -> Option<Rev> {
    let rv = Paragraph(p).mark_revision(d)?;
    let kind = match rv.kind {
        RevisionKind::ParaMarkIns => RevKind::Ins,
        RevisionKind::ParaMarkDel => RevKind::Del,
        _ => return None,
    };
    Some(Rev { id: rv.id(d), kind, author: rv.author(d).unwrap_or_default(), date: rv.date(d), moved_from: None })
}

impl Builder<'_, '_> {
    fn push(&mut self, kind: RawKind, run: Option<NodeId>, c: &ICtx) {
        let comments = self.st.current();
        self.raw.push(Raw { kind, run, rev: c.rev.clone(), link: c.link.clone(), deleted: c.deleted, comments });
    }

    fn container(&mut self, n: NodeId, c: &ICtx) {
        let d = self.d;
        let mut ch = d.first_child(n);
        while let Some(x) = ch {
            ch = d.next_sibling(x);
            if d.is_element_node(x) {
                self.element(x, c);
            }
        }
    }

    fn alternate(&mut self, x: NodeId, f: &mut dyn FnMut(&mut Self, NodeId)) {
        let d = self.d;
        for ch in d.child_elements(x).collect::<Vec<_>>() {
            if !is_ignored_branch(d, ch) {
                f(self, ch);
                return;
            }
        }
    }

    fn element(&mut self, x: NodeId, c: &ICtx) {
        let d = self.d;
        match d.namespace(x) {
            Some(ns::MC) => {
                if d.local_name(x) == Some("AlternateContent") {
                    self.alternate(x, &mut |b, ch| b.container(ch, c));
                }
                return;
            }
            Some(ns::M) => {
                if matches!(d.local_name(x), Some("oMath" | "oMathPara")) {
                    let t = text(d, x, TextMode::AcceptAll);
                    if !t.is_empty() {
                        self.push(RawKind::Atom(Atom::Text(t)), None, c);
                    }
                }
                return;
            }
            Some(ns::W) => {}
            _ => return,
        }
        let local = d.local_name(x).unwrap_or("");
        match local {
            "r" => self.run(x, c),
            "hyperlink" => {
                let link = if let Some(rid) = d.attr_ns(x, ns::R, "id") {
                    self.cx
                        .doc
                        .rel(self.part, &rid)
                        .filter(|r| r.external)
                        .map(|r| r.target.clone())
                        .filter(|u| safe_url(u))
                        .map(Link::Url)
                } else {
                    w_attr(d, x, "anchor").filter(|a| !a.is_empty()).map(Link::Bookmark)
                };
                let mut c2 = c.clone();
                if let Some(l) = link {
                    c2.link = Some(Rc::new(l));
                }
                self.container(x, &c2);
            }
            "ins" | "del" | "moveFrom" | "moveTo" => {
                let kind = match local {
                    "ins" => RevKind::Ins,
                    "del" => RevKind::Del,
                    "moveFrom" => RevKind::MoveFrom,
                    _ => RevKind::MoveTo,
                };
                let author = w_attr(d, x, "author").unwrap_or_default();
                let id = w_attr(d, x, "id").and_then(|v| v.trim().parse().ok());
                let moved_from = if kind == RevKind::MoveTo {
                    self.cx.move_source(Loc::new(self.part, x)).map(str::to_owned)
                } else {
                    None
                };
                let rev = Rev { id, kind, author, date: w_attr(d, x, "date"), moved_from };
                let mut c2 = c.clone();
                c2.deleted |= kind.is_del();
                c2.rev = Some(Rc::new(rev));
                self.container(x, &c2);
            }
            "fldSimple" => {
                let first_run = d.descendants_named(x, ns::W, "r").next();
                self.push(RawKind::Begin, first_run, c);
                self.push(RawKind::Instr(w_attr(d, x, "instr").unwrap_or_default()), first_run, c);
                self.push(RawKind::Sep, first_run, c);
                self.container(x, c);
                self.push(RawKind::End, first_run, c);
            }
            "sdt" => {
                let ctl = Control::of(d, x);
                let first_run =
                    d.find_child(x, ns::W, "sdtContent").and_then(|sc| d.descendants_named(sc, ns::W, "r").next());
                self.push(RawKind::Atom(Atom::Control(Box::new(ctl))), first_run, c);
            }
            "commentRangeStart" => {
                if let Some(id) = comment_id(d, x)
                    && self.cx.comment(id).is_some_and(|ci| ci.parent.is_none())
                    && !self.st.active.contains(&id)
                {
                    self.st.active.push(id);
                    self.st.snapshot = None;
                }
            }
            "commentRangeEnd" => {
                if let Some(id) = comment_id(d, x) {
                    if let Some(pos) = self.st.active.iter().position(|&a| a == id) {
                        self.st.active.remove(pos);
                        self.st.snapshot = None;
                    }
                    if self.cx.comment(id).is_some_and(|ci| ci.parent.is_none()) {
                        self.push(RawKind::Atom(Atom::Comment(id)), None, c);
                    }
                }
            }
            l if is_props(l) => {}
            "bookmarkStart"
            | "bookmarkEnd"
            | "proofErr"
            | "permStart"
            | "permEnd"
            | "moveFromRangeStart"
            | "moveFromRangeEnd"
            | "moveToRangeStart"
            | "moveToRangeEnd"
            | "customXmlInsRangeStart"
            | "customXmlInsRangeEnd"
            | "customXmlDelRangeStart"
            | "customXmlDelRangeEnd"
            | "customXmlMoveFromRangeStart"
            | "customXmlMoveFromRangeEnd"
            | "customXmlMoveToRangeStart"
            | "customXmlMoveToRangeEnd" => {}
            _ => self.container(x, c),
        }
    }

    fn run(&mut self, r: NodeId, c: &ICtx) {
        let d = self.d;
        let mut ch = d.first_child(r);
        while let Some(x) = ch {
            ch = d.next_sibling(x);
            if d.is_element_node(x) {
                self.run_child(r, x, c);
            }
        }
    }

    fn run_child(&mut self, r: NodeId, x: NodeId, c: &ICtx) {
        let d = self.d;
        let atom = |b: &mut Self, a: Atom| b.push(RawKind::Atom(a), Some(r), c);
        match d.namespace(x) {
            Some(ns::W) => {}
            Some(ns::MC) if d.local_name(x) == Some("AlternateContent") => {
                self.alternate(x, &mut |b, branch| {
                    for y in b.d.child_elements(branch).collect::<Vec<_>>() {
                        b.run_child(r, y, c);
                    }
                });
                return;
            }
            _ => {
                let t = text(d, x, TextMode::AcceptAll);
                if !t.is_empty() {
                    atom(self, Atom::Text(t));
                }
                return;
            }
        }
        match d.local_name(x).unwrap_or("") {
            "t" | "delText" => {
                let t = d.text_content(x);
                if !t.is_empty() {
                    atom(self, Atom::Text(t));
                }
            }
            "instrText" | "delInstrText" => {
                self.push(RawKind::Instr(d.text_content(x)), Some(r), c);
            }
            "fldChar" => {
                let k = match w_attr(d, x, "fldCharType").as_deref() {
                    Some("begin") => RawKind::Begin,
                    Some("separate") => RawKind::Sep,
                    Some("end") => RawKind::End,
                    _ => return,
                };
                self.push(k, Some(r), c);
            }
            "tab" | "ptab" => atom(self, Atom::Tab),
            "br" => {
                let k = match w_attr(d, x, "type").as_deref() {
                    Some("page") => BrKind::Page,
                    Some("column") => BrKind::Column,
                    _ => BrKind::Line,
                };
                atom(self, Atom::Br(k));
            }
            "cr" => atom(self, Atom::Br(BrKind::Line)),
            "noBreakHyphen" => atom(self, Atom::Text("\u{2011}".into())),
            "softHyphen" => atom(self, Atom::Text("\u{AD}".into())),
            "sym" => {
                if let Some(code) = w_attr(d, x, "char").and_then(|v| u32::from_str_radix(v.trim(), 16).ok()) {
                    if (0xE000..=0xF8FF).contains(&code) {
                        atom(self, Atom::Sym(code));
                    } else if let Some(ch) = char::from_u32(code) {
                        atom(self, Atom::Text(ch.to_string()));
                    }
                }
            }
            "footnoteReference" | "endnoteReference" => {
                if let Some(id) = w_attr(d, x, "id").and_then(|v| v.trim().parse().ok()) {
                    let endnote = d.local_name(x) == Some("endnoteReference");
                    atom(self, Atom::Note { endnote, id });
                }
            }
            "commentReference" => {
                if let Some(id) = comment_id(d, x)
                    && !self.cx.has_range_end(id)
                    && self.cx.comment(id).is_some_and(|ci| ci.parent.is_none())
                {
                    atom(self, Atom::Comment(id));
                }
            }
            "drawing" | "pict" | "object" => atom(self, Atom::Image(x)),
            l if is_props(l) => {}
            "lastRenderedPageBreak"
            | "annotationRef"
            | "footnoteRef"
            | "endnoteRef"
            | "separator"
            | "continuationSeparator" => {}
            _ => {
                let t = text(d, x, TextMode::AcceptAll);
                if !t.is_empty() {
                    atom(self, Atom::Text(t));
                }
            }
        }
    }
}

fn comment_id(d: &Document, x: NodeId) -> Option<i64> {
    w_attr(d, x, "id").and_then(|v| v.trim().parse().ok())
}

/// http(s) and mailto only.
pub(crate) fn safe_url(u: &str) -> bool {
    let l = u.trim().to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l.starts_with("mailto:")
}

/// Accepted-mode text a raw item contributes.
fn raw_plain(r: &Raw) -> String {
    if r.deleted {
        return String::new();
    }
    match &r.kind {
        RawKind::Atom(a) => atom_plain(a),
        _ => String::new(),
    }
}

pub(crate) fn atom_plain(a: &Atom) -> String {
    match a {
        Atom::Text(t) => t.clone(),
        Atom::Tab => "\t".into(),
        Atom::Br(_) => "\n".into(),
        Atom::Sym(c) => char::from_u32(*c).map(String::from).unwrap_or_default(),
        Atom::Control(c) => match &c.value {
            CtlValue::Text(t) => t.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// Fold complex fields closed within the paragraph into field atoms and turn raw items into
/// pieces.
fn fold(cx: &DocCtx, part: PartIdx, p: NodeId, raw: Vec<Raw>) -> Vec<Piece> {
    let d = cx.doc.part(part);
    // Match begin/end pairs.
    let mut pair: Vec<Option<usize>> = vec![None; raw.len()];
    let mut stack = Vec::new();
    for (i, r) in raw.iter().enumerate() {
        match r.kind {
            RawKind::Begin => stack.push(i),
            RawKind::End => {
                if let Some(b) = stack.pop() {
                    pair[b] = Some(i);
                }
            }
            _ => {}
        }
    }
    let mut runs: HashMap<NodeId, Rc<RunInfo>> = HashMap::new();
    let base = cx.base_run(part, p);
    let mut run_info = |r: Option<NodeId>| -> Option<Rc<RunInfo>> {
        let r = r?;
        if let Some(i) = runs.get(&r) {
            return Some(i.clone());
        }
        let er = cx.run(part, p, r);
        let marks = Marks::relative(&er, &base);
        let s = er.script.unwrap_or(Script::Ascii);
        let fence = if er.vanish == Some(true) || er.spec_vanish == Some(true) {
            Some("hidden")
        } else if is_white(cx, part, p, &er) {
            Some("white")
        } else if er.size_for(s).is_some_and(|z| z < 4) {
            Some("tiny")
        } else {
            None
        };
        let fmt = Run(r).property_change(d).map(|rv| {
            Rc::new(FmtChange {
                id: rv.id(d),
                author: rv.author(d).unwrap_or_default(),
                date: rv.date(d),
                desc: fmt_desc(d, r, rv),
            })
        });
        let info = Rc::new(RunInfo { marks: Rc::new(marks), style: er.style.clone().map(Rc::from), fence, fmt });
        runs.insert(r, info.clone());
        Some(info)
    };

    let mut out = Vec::new();
    let mut i = 0;
    // Unclosed fields (spanning paragraphs): true while in their code phase.
    let mut open_code = 0usize;
    while i < raw.len() {
        let r = &raw[i];
        match &r.kind {
            RawKind::Begin => {
                if let Some(e) = pair[i] {
                    let (instr, result, plain) = fold_field(&raw, &pair, i, e);
                    let fa = FieldAtom {
                        link: hyperlink_target(&instr),
                        dirty: false,
                        instr: instr.trim().to_owned(),
                        result,
                    };
                    let info = run_info(r.run);
                    push_piece(cx, &mut out, Atom::Field(Box::new(fa)), plain, r, info.as_deref());
                    i = e + 1;
                    continue;
                }
                open_code += 1;
            }
            RawKind::Sep => open_code = open_code.saturating_sub(1),
            RawKind::End | RawKind::Instr(_) => {}
            RawKind::Atom(a) => {
                if open_code == 0 {
                    let info = run_info(r.run);
                    push_piece(cx, &mut out, a.clone(), raw_plain(r), r, info.as_deref());
                }
            }
        }
        i += 1;
    }
    out
}

/// (instruction, displayed result, accepted plain text) of the field `b..=e`.
fn fold_field(raw: &[Raw], pair: &[Option<usize>], b: usize, e: usize) -> (String, String, String) {
    let (mut instr, mut result, mut plain) = (String::new(), String::new(), String::new());
    let mut code = true;
    let mut j = b + 1;
    while j < e {
        let r = &raw[j];
        match &r.kind {
            RawKind::Begin => {
                if let Some(e2) = pair[j] {
                    let (_, r2, p2) = fold_field(raw, pair, j, e2);
                    if code {
                        instr.push_str(&r2);
                    } else if !r.deleted {
                        result.push_str(&r2);
                    }
                    plain.push_str(&p2);
                    j = e2 + 1;
                    continue;
                }
            }
            RawKind::Sep => code = false,
            RawKind::Instr(s) => {
                if code {
                    instr.push_str(s);
                }
            }
            RawKind::End => {}
            RawKind::Atom(_) => {
                let t = raw_plain(r);
                if !code {
                    result.push_str(&t);
                }
                plain.push_str(&t);
            }
        }
        j += 1;
    }
    (instr, result, plain)
}

fn hyperlink_target(instr: &str) -> Option<Link> {
    let mut words = instr.split_whitespace();
    if !words.next()?.eq_ignore_ascii_case("HYPERLINK") {
        return None;
    }
    let rest: Vec<&str> = words.collect();
    let unq = |s: &str| s.trim_matches('"').to_owned();
    let mut k = 0;
    let mut url = None;
    let mut bm = None;
    while k < rest.len() {
        let w = rest[k];
        if w.eq_ignore_ascii_case("\\l") {
            bm = rest.get(k + 1).map(|s| unq(s));
            k += 2;
            continue;
        }
        if w.starts_with('\\') {
            k += if matches!(w, "\\o" | "\\t") { 2 } else { 1 };
            continue;
        }
        if url.is_none() {
            url = Some(unq(w));
        }
        k += 1;
    }
    match (url, bm) {
        (Some(u), _) if safe_url(&u) => Some(Link::Url(u)),
        (_, Some(b)) if !b.is_empty() => Some(Link::Bookmark(b)),
        _ => None,
    }
}

fn push_piece(cx: &DocCtx, out: &mut Vec<Piece>, atom: Atom, plain: String, r: &Raw, info: Option<&RunInfo>) {
    // Revisions the view applies silently.
    let mut rev = r.rev.clone();
    if let Some(rv) = &rev
        && !cx.shows(&rv.author, rv.id)
    {
        if rv.kind.is_del() && !matches!(atom, Atom::Comment(_)) {
            return;
        }
        rev = None;
    }
    let fmt = info.and_then(|i| i.fmt.clone()).filter(|f| cx.shows(&f.author, f.id));
    let visible = match &atom {
        Atom::Text(t) => t.chars().any(|c| !c.is_whitespace()),
        Atom::Field(f) => f.result.chars().any(|c| !c.is_whitespace()),
        Atom::Control(_) => true,
        _ => false,
    };
    let fence = if visible { info.and_then(|i| i.fence) } else { None };
    if let Some(f) = fence {
        cx.flag(match f {
            "hidden" => "hidden_text",
            "white" => "white_text",
            _ => "tiny_text",
        });
    }
    let zw = matches!(&atom, Atom::Text(t) if t.chars().any(is_zero_width));
    let (marks, style) = match (&atom, info) {
        (Atom::Comment(_) | Atom::ParaMark, _) | (_, None) => (Rc::default(), None),
        (_, Some(i)) => (i.marks.clone(), i.style.clone()),
    };
    out.push(Piece {
        atom,
        plain,
        marks,
        style,
        rev,
        fmt,
        link: r.link.clone(),
        fence,
        zw,
        comments: r.comments.clone(),
    });
}

pub(crate) fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}')
}

fn is_white(cx: &DocCtx, part: PartIdx, p: NodeId, er: &EffectiveRun) -> bool {
    let Some(color) = er.color.as_deref().filter(|c| *c != "auto" && c.len() == 6) else { return false };
    let bg = er
        .shading
        .as_ref()
        .and_then(|s| s.fill.clone())
        .filter(|f| f != "auto" && f.len() == 6)
        .or_else(|| er.highlight.as_deref().and_then(highlight_rgb).map(str::to_owned))
        .or_else(|| cx.para_background(part, p))
        .unwrap_or_else(|| "FFFFFF".to_owned());
    delta_e(color, &bg).is_some_and(|e| e <= 3.0)
}

/// `+bold -italic font` from a run's `w:rPr` against its `w:rPrChange/w:rPr`.
fn fmt_desc(d: &Document, r: NodeId, rv: Revision) -> String {
    let new = Run(r).rpr(d);
    let old = d.find_child(rv.node, ns::W, "rPr");
    let on = |n: Option<NodeId>, k: &str| n.is_some_and(|n| child_on(d, n, k));
    let val = |n: Option<NodeId>, k: &str| n.and_then(|n| child_val(d, n, k));
    let present = |n: Option<NodeId>, k: &str| n.and_then(|n| d.find_child(n, ns::W, k)).map(|c| d.node_to_bytes(c));
    let mut parts = Vec::new();
    for (k, name) in [
        ("b", "bold"),
        ("i", "italic"),
        ("strike", "strike"),
        ("dstrike", "dstrike"),
        ("caps", "caps"),
        ("smallCaps", "smallcaps"),
        ("vanish", "hidden"),
    ] {
        match (on(new, k), on(old, k)) {
            (true, false) => parts.push(format!("+{name}")),
            (false, true) => parts.push(format!("-{name}")),
            _ => {}
        }
    }
    let ul = |n| val(n, "u").filter(|v| v != "none");
    match (ul(new).is_some(), ul(old).is_some()) {
        (true, false) => parts.push("+underline".into()),
        (false, true) => parts.push("-underline".into()),
        _ => {}
    }
    for (v, name) in [("superscript", "superscript"), ("subscript", "subscript")] {
        let a = val(new, "vertAlign").as_deref() == Some(v);
        let b = val(old, "vertAlign").as_deref() == Some(v);
        if a != b {
            parts.push(format!("{}{name}", if a { "+" } else { "-" }));
        }
    }
    for (k, name) in
        [("rFonts", "font"), ("sz", "size"), ("color", "color"), ("highlight", "highlight"), ("rStyle", "style")]
    {
        if present(new, k) != present(old, k) {
            parts.push(name.into());
        }
    }
    parts.join(" ")
}

/// Merge adjacent text pieces that render alike.
fn merge(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::with_capacity(pieces.len());
    for p in pieces {
        if let (Some(last), Atom::Text(t)) = (out.last_mut(), &p.atom)
            && let Atom::Text(lt) = &mut last.atom
            && last.marks == p.marks
            && last.style == p.style
            && last.rev == p.rev
            && last.fmt == p.fmt
            && last.link == p.link
            && last.fence == p.fence
            && last.comments == p.comments
        {
            lt.push_str(t);
            last.plain.push_str(&p.plain);
            last.zw |= p.zw;
            continue;
        }
        out.push(p);
    }
    out
}

// ----------------------------------------------------------------------------- rendering

/// Quote-escape a value printed inside `"…"`.
pub(crate) fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\t' => o.push('⇥'),
            '\n' | '\r' => o.push('↵'),
            c => o.push(c),
        }
    }
    o
}

/// Escape literal text so it can't be read as markup (§4 Escaping).
pub(crate) fn escape(s: &str, in_table: bool, o: &mut String) {
    let chars: Vec<char> = s.chars().collect();
    for (k, &c) in chars.iter().enumerate() {
        match c {
            '*' | '^' | '~' | '[' | '\\' | '⟪' | '⟦' | '⟨' => {
                o.push('\\');
                o.push(c);
            }
            '{' if matches!(chars.get(k + 1), Some('+' | '-' | '~' | '=' | '>')) => o.push_str("\\{"),
            '|' if in_table => o.push_str("\\|"),
            '_' => {
                // Only a pair `__` can be read as underline; longer runs are fill lines.
                let mut a = k;
                while a > 0 && chars[a - 1] == '_' {
                    a -= 1;
                }
                let mut b = k;
                while b + 1 < chars.len() && chars[b + 1] == '_' {
                    b += 1;
                }
                if b - a == 1 {
                    o.push('\\');
                }
                o.push('_');
            }
            '\t' => o.push('⇥'),
            '\n' | '\r' => o.push('↵'),
            c => o.push(c),
        }
    }
}

/// Inline rendering options.
#[derive(Clone, Copy, Default)]
pub(crate) struct RenderOpts {
    pub in_table: bool,
}

/// Render pieces as one projection line's text.
pub(crate) fn render(cx: &DocCtx, part: PartIdx, pieces: &[Piece], ro: RenderOpts) -> String {
    let keyed: Vec<Keyed> = pieces.iter().map(|p| Keyed::of(cx, p)).collect();
    let refs: Vec<&Keyed> = keyed.iter().collect();
    let mut out = String::new();
    level(cx, part, &refs, 0, ro, &mut out);
    out
}

struct Keyed<'p> {
    p: &'p Piece,
    keys: [Option<String>; 5],
    marks: [bool; 6],
}

const LEVELS: usize = 11;
const MARKS: [(&str, &str); 6] = [("**", "**"), ("*", "*"), ("__", "__"), ("~~", "~~"), ("^", "^"), ("~", "~")];

impl<'p> Keyed<'p> {
    fn of(cx: &DocCtx, p: &'p Piece) -> Self {
        let rev_group = p.rev.as_ref().map(|r| {
            let cs = r.id.and_then(|i| cx.change_set(i)).map(|c| c.id.as_str()).unwrap_or("");
            format!("{}\u{1}{}\u{1}{}", r.author, cs, r.moved_from.as_deref().unwrap_or(""))
        });
        let kind = p.rev.as_ref().map(|r| if r.kind.is_del() { "--".to_owned() } else { "++".to_owned() });
        let fmt = p.fmt.as_ref().map(|f| format!("{}\u{1}{:?}\u{1}{}", f.desc, f.id, f.author));
        let fence = p.fence.map(str::to_owned);
        let link = p.link.as_ref().map(|l| match l.as_ref() {
            Link::Url(u) => u.clone(),
            Link::Bookmark(b) => format!("#{b}"),
        });
        let m = &p.marks;
        let text_like = !matches!(p.atom, Atom::Comment(_) | Atom::ParaMark | Atom::Note { .. });
        Keyed {
            p,
            keys: [rev_group, kind, fmt, fence, link],
            marks: [
                text_like && m.b == Some(true),
                text_like && m.i == Some(true),
                text_like && m.u.is_some() && p.link.is_none(),
                text_like && (m.strike || m.dstrike),
                text_like && m.sup,
                text_like && m.sub,
            ],
        }
    }

    fn key(&self, level: usize) -> Option<&str> {
        if level < 5 {
            self.keys[level].as_deref()
        } else if self.marks[level - 5] {
            Some("")
        } else {
            None
        }
    }
}

fn level(cx: &DocCtx, part: PartIdx, ps: &[&Keyed], lvl: usize, ro: RenderOpts, out: &mut String) {
    if lvl == LEVELS {
        for k in ps {
            atom(cx, part, k.p, ro, out);
        }
        return;
    }
    let mut i = 0;
    while i < ps.len() {
        let key = ps[i].key(lvl);
        let mut j = i + 1;
        while j < ps.len() && ps[j].key(lvl) == key {
            j += 1;
        }
        let group = &ps[i..j];
        if key.is_none() {
            level(cx, part, group, lvl + 1, ro, out);
        } else {
            match lvl {
                0 => {
                    level(cx, part, group, 1, ro, out);
                    let r = group[0].p.rev.as_ref().expect("keyed");
                    let mut ids: Vec<i64> = group.iter().filter_map(|k| k.p.rev.as_ref().and_then(|r| r.id)).collect();
                    ids.sort_unstable();
                    ids.dedup();
                    out.push('⟨');
                    if !ids.is_empty() {
                        out.push('r');
                        out.push_str(&ids.iter().map(i64::to_string).collect::<Vec<_>>().join(","));
                        out.push(' ');
                    }
                    out.push_str(&cx.author_label(&r.author));
                    if let Some(m) = &r.moved_from {
                        out.push_str(" moved from ");
                        out.push_str(m);
                    }
                    out.push('⟩');
                }
                1 => {
                    let k = key.unwrap_or("++");
                    out.push('{');
                    out.push_str(k);
                    level(cx, part, group, 2, ro, out);
                    out.push_str(k);
                    out.push('}');
                }
                2 => {
                    let f = group[0].p.fmt.as_ref().expect("keyed");
                    out.push_str("{==");
                    level(cx, part, group, 3, ro, out);
                    out.push_str("==}⟨fmt ");
                    if !f.desc.is_empty() {
                        out.push_str(&f.desc);
                        out.push(' ');
                    }
                    if let Some(id) = f.id {
                        out.push_str(&format!("r{id} "));
                    }
                    out.push_str(&cx.author_label(&f.author));
                    out.push('⟩');
                }
                3 => {
                    out.push('⟦');
                    out.push_str(key.unwrap_or("hidden"));
                    out.push('⟧');
                    level(cx, part, group, 4, ro, out);
                    out.push_str("⟦/⟧");
                }
                4 => {
                    let mut inner = String::new();
                    level(cx, part, group, 5, ro, &mut inner);
                    // A field that is itself a link already rendered as one.
                    if inner.starts_with('[')
                        && inner.ends_with(')')
                        && group.len() == 1
                        && matches!(group[0].p.atom, Atom::Field(_))
                    {
                        out.push_str(&inner);
                    } else {
                        out.push('[');
                        out.push_str(&inner);
                        out.push_str("](");
                        out.push_str(key.unwrap_or(""));
                        out.push(')');
                    }
                }
                _ => {
                    let (open, close) = MARKS[lvl - 5];
                    let mut inner = String::new();
                    level(cx, part, group, lvl + 1, ro, &mut inner);
                    let core = inner.trim_matches(|c: char| c == ' ' || c == '\u{a0}');
                    if core.is_empty() {
                        out.push_str(&inner);
                    } else {
                        let lead = &inner[..inner.len() - inner.trim_start_matches([' ', '\u{a0}']).len()];
                        let trail = &inner[inner.trim_end_matches([' ', '\u{a0}']).len()..];
                        out.push_str(lead);
                        out.push_str(open);
                        out.push_str(core);
                        out.push_str(close);
                        out.push_str(trail);
                    }
                }
            }
        }
        i = j;
    }
}

fn atom(cx: &DocCtx, part: PartIdx, p: &Piece, ro: RenderOpts, out: &mut String) {
    match &p.atom {
        Atom::Text(t) => escape(t, ro.in_table, out),
        Atom::Tab => out.push('⇥'),
        Atom::Br(BrKind::Line) => out.push('↵'),
        Atom::Br(BrKind::Page) => out.push_str("⟨page⟩"),
        Atom::Br(BrKind::Column) => out.push_str("⟨col⟩"),
        Atom::Field(f) => match &f.link {
            Some(l) => {
                out.push('[');
                escape(&f.result, ro.in_table, out);
                out.push_str("](");
                match l {
                    Link::Url(u) => out.push_str(u),
                    Link::Bookmark(b) => {
                        out.push('#');
                        out.push_str(b);
                    }
                }
                out.push(')');
            }
            None => {
                out.push('⟪');
                out.push_str(&f.typ());
                let args = f.args();
                if !args.is_empty() {
                    out.push(' ');
                    out.push_str(&args);
                }
                if !f.result.is_empty() {
                    out.push_str(" → \"");
                    out.push_str(&quote(&f.result));
                    out.push('"');
                }
                out.push('⟫');
            }
        },
        Atom::Control(c) => out.push_str(&c.render()),
        Atom::Image(n) => {
            let loc = Loc::new(part, *n);
            let id = cx.anchors.handle_of(loc).unwrap_or("?");
            let info = super::image::info(cx, part, *n);
            out.push_str("![");
            if let Some(a) = &info.alt {
                let mut e = String::new();
                escape(a, ro.in_table, &mut e);
                out.push_str(&e.replace(']', "\\]"));
            }
            out.push_str("](");
            out.push_str(id);
            if let Some(tb) = cx.textboxes.get(&loc) {
                out.push_str(" → ");
                out.push_str(tb);
            }
            out.push(')');
        }
        Atom::Note { endnote, id } => {
            out.push_str("[^");
            out.push_str(cx.note(*endnote, *id).unwrap_or("?"));
            out.push(']');
        }
        Atom::Comment(id) => {
            out.push_str("{>>");
            out.push_str(cx.comment(*id).map(|c| c.sid.as_str()).unwrap_or("c?"));
            out.push_str("<<}");
        }
        Atom::ParaMark => out.push('¶'),
        Atom::Sym(c) => out.push_str(&format!("⟨sym {c:04X}⟩")),
    }
}
