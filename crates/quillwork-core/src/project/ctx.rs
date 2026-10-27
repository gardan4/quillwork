//! Document context: everything the renderers look up more than once (list labels, comments,
//! notes, bookmarks, moves, change sets). The document-wide part ([`DocIndex`]) is built once
//! per call, or once per document version through [`super::Projector`].

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use crate::anchors::Anchors;
use crate::wml::style::{EffectiveParagraph, EffectiveRun, StyleSheet};
use crate::wml::tree::{Step, child_on, is_ignored_branch, is_props, w_attr, w_local, walk};
use crate::wml::view::{self, Paragraph};
use crate::wml::{ListLabel, Loc, Numbering, PartIdx, Story, StoryKind, TextMode, WordDoc};
use crate::xml::{Document, NodeId, ns};

use super::{ChangeSetInfo, ProjectOptions, View};

/// Styles used when a package has no `styles.xml`.
pub(crate) const MIN_STYLES: &str = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="20"/></w:rPr></w:rPrDefault><w:pPrDefault/></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style></w:styles>"#;

/// One comment (`w:comment`) with what the renderers need.
#[derive(Debug, Clone)]
pub(crate) struct CommentInfo {
    /// `w:comment/@w:id`.
    pub wid: i64,
    /// Story id (`c1`).
    pub sid: String,
    /// Index into `doc.stories()`.
    pub story: usize,
    pub author: String,
    pub initials: Option<String>,
    pub date: Option<String>,
    pub done: bool,
    /// Story id of the parent comment (replies).
    pub parent: Option<String>,
}

/// Document-wide lookups that depend only on the document and its anchors (list labels,
/// comments, notes, text boxes, bookmarks, moves, page background). Building it walks the whole
/// document, so a host that renders many small reads of one version keeps it (see
/// [`super::Projector`]); it is stale as soon as the document changes.
#[derive(Debug, Clone, Default)]
pub(crate) struct DocIndex {
    labels: HashMap<Loc, ListLabel>,
    pub comments: Vec<CommentInfo>,
    comment_by_wid: HashMap<i64, usize>,
    /// Comment ids that have a `w:commentRangeEnd` somewhere.
    range_ended: HashSet<i64>,
    /// (endnote?, `w:id`) → story id.
    notes: HashMap<(bool, i64), String>,
    /// Host drawing node (`w:drawing` / `w:pict`) → text-box story id.
    pub textboxes: HashMap<Loc, String>,
    /// Bookmark name → anchor of the paragraph it starts in.
    pub bookmarks: HashMap<String, String>,
    /// `w:moveTo` wrapper → anchor of the paragraph its source (`w:moveFrom`) range starts in.
    moves: HashMap<Loc, String>,
    /// `w:background/@w:color`, `RRGGBB`.
    pub background: Option<String>,
}

impl DocIndex {
    pub fn build(doc: &WordDoc, anchors: &Anchors) -> DocIndex {
        let numbering = Numbering::load(doc);
        let mut labels = HashMap::new();
        for st in doc.stories() {
            for (p, l) in numbering.labels(doc, st) {
                if let Some(l) = l {
                    labels.insert(Loc::new(st.part, p.0), l);
                }
            }
        }
        let mut ix = DocIndex { labels, ..DocIndex::default() };
        ix.scan_stories(doc, anchors);
        ix.load_comments(doc, anchors);
        let md = doc.part(doc.main_part());
        ix.background = md
            .find_child(md.root(), ns::W, "background")
            .and_then(|b| w_attr(md, b, "color"))
            .filter(|c| c.len() == 6 && c.chars().all(|ch| ch.is_ascii_hexdigit()))
            .map(|c| c.to_ascii_uppercase());
        ix
    }

    fn scan_stories(&mut self, doc: &WordDoc, anchors: &Anchors) {
        for (si, st) in doc.stories().iter().enumerate() {
            let sid = anchors.story_id(si).unwrap_or("?").to_owned();
            match st.kind {
                StoryKind::Footnote { id } => {
                    self.notes.insert((false, id), sid.clone());
                }
                StoryKind::Endnote { id } => {
                    self.notes.insert((true, id), sid.clone());
                }
                StoryKind::TextBox { .. } => {
                    let d = doc.part(st.part);
                    if let Some(host) =
                        d.ancestors(st.root).find(|&a| matches!(w_local(d, a), Some("drawing" | "pict" | "object")))
                    {
                        self.textboxes.insert(Loc::new(st.part, host), sid.clone());
                    }
                }
                _ => {}
            }
            if matches!(st.kind, StoryKind::TextBox { .. }) {
                continue;
            }
            // Bookmarks, moves and comment range ends, in one walk.
            let d = doc.part(st.part);
            let mut move_names: HashMap<String, String> = HashMap::new(); // moveToRangeStart w:id → name
            let mut open_move_to: Option<String> = None;
            let mut from_src: Vec<(String, Loc)> = Vec::new();
            let mut to_nodes: Vec<(String, NodeId)> = Vec::new();
            let part = st.part;
            let mut bms: Vec<(String, NodeId)> = Vec::new();
            walk(d, st.root, |n| {
                if is_ignored_branch(d, n) {
                    return Step::Over;
                }
                match w_local(d, n) {
                    Some("txbxContent") => Step::Over,
                    Some(l) if is_props(l) => Step::Over,
                    Some("bookmarkStart") => {
                        if let Some(name) = w_attr(d, n, "name") {
                            bms.push((name, n));
                        }
                        Step::Over
                    }
                    Some("commentRangeEnd") => {
                        if let Some(id) = w_attr(d, n, "id").and_then(|v| v.trim().parse().ok()) {
                            self.range_ended.insert(id);
                        }
                        Step::Over
                    }
                    Some("moveFromRangeStart") => {
                        if let Some(name) = w_attr(d, n, "name") {
                            from_src.push((name, Loc::new(part, n)));
                        }
                        Step::Over
                    }
                    Some("moveToRangeStart") => {
                        let name = w_attr(d, n, "name").unwrap_or_default();
                        if let Some(id) = w_attr(d, n, "id") {
                            move_names.insert(id, name.clone());
                        }
                        open_move_to = Some(name);
                        Step::Over
                    }
                    Some("moveToRangeEnd") => {
                        open_move_to = None;
                        Step::Over
                    }
                    Some("moveTo") => {
                        if let Some(name) = &open_move_to {
                            to_nodes.push((name.clone(), n));
                        }
                        Step::Into
                    }
                    _ => Step::Into,
                }
            });
            for (name, n) in bms {
                if let Some(a) = block_anchor_near(doc, anchors, part, n) {
                    self.bookmarks.entry(name).or_insert(a);
                }
            }
            let src: HashMap<String, String> = from_src
                .into_iter()
                .filter_map(|(name, loc)| Some((name, block_anchor_near(doc, anchors, loc.part, loc.node)?)))
                .collect();
            for (name, n) in to_nodes {
                if let Some(a) = src.get(&name) {
                    self.moves.insert(Loc::new(part, n), a.clone());
                }
            }
        }
    }

    fn load_comments(&mut self, doc: &WordDoc, anchors: &Anchors) {
        // commentsExtended: paraId → (done, paraIdParent).
        let mut ext: HashMap<String, (bool, Option<String>)> = HashMap::new();
        if let Some(cp) = doc.comments_part() {
            let name = doc
                .rels(doc.main_part())
                .iter()
                .chain(doc.rels(cp).iter())
                .find(|r| r.rel_type.ends_with("/commentsExtended") && !r.external)
                .and_then(|r| crate::opc::resolve_target(doc.part_name(doc.main_part()), &r.target));
            // The live part when the document holds it (an edit wrote replies or the done
            // flag there), else the part as uploaded.
            let parsed;
            let x = match name.as_deref().and_then(|n| doc.part_idx(n)) {
                Some(i) => Some(doc.part(i)),
                None => {
                    parsed = name
                        .and_then(|n| doc.package().and_then(|pkg| pkg.part(&n).ok()))
                        .and_then(|bytes| Document::parse(&bytes).ok());
                    parsed.as_ref()
                }
            };
            if let Some(x) = x {
                for c in x.descendants(x.root()) {
                    if x.local_name(c) == Some("commentEx") {
                        let pid = x.attr_ns(c, ns::W15, "paraId").map(|v| v.to_ascii_uppercase());
                        let parent = x.attr_ns(c, ns::W15, "paraIdParent").map(|v| v.to_ascii_uppercase());
                        let done = matches!(x.attr_ns(c, ns::W15, "done").as_deref(), Some("1" | "true" | "on"));
                        if let Some(pid) = pid {
                            ext.insert(pid, (done, parent));
                        }
                    }
                }
            }
        }
        // paraId of a comment's last paragraph → its story id (to resolve paraIdParent).
        let mut by_para: HashMap<String, String> = HashMap::new();
        let mut raw = Vec::new();
        for (si, st) in doc.stories().iter().enumerate() {
            let StoryKind::Comment { id } = st.kind else { continue };
            let d = doc.part(st.part);
            let sid = anchors.story_id(si).unwrap_or("?").to_owned();
            let last_pid = view::paragraphs(d, st.root)
                .last()
                .and_then(|p| d.attr_ns(p.0, ns::W14, "paraId").map(|v| v.to_ascii_uppercase()));
            if let Some(pid) = &last_pid {
                by_para.insert(pid.clone(), sid.clone());
            }
            raw.push((si, st.part, st.root, id, sid, last_pid));
        }
        for (si, part, root, wid, sid, last_pid) in raw {
            let d = doc.part(part);
            let (done, parent_pid) = last_pid.as_ref().and_then(|p| ext.get(p).cloned()).unwrap_or((false, None));
            let parent = parent_pid.and_then(|p| by_para.get(&p).cloned()).filter(|p| *p != sid);
            self.comment_by_wid.insert(wid, self.comments.len());
            self.comments.push(CommentInfo {
                wid,
                sid,
                story: si,
                author: w_attr(d, root, "author").unwrap_or_default(),
                initials: w_attr(d, root, "initials").filter(|s| !s.is_empty()),
                date: w_attr(d, root, "date").filter(|s| !s.is_empty()),
                done,
                parent,
            });
        }
    }
}

/// Anchor of the paragraph `n` sits in, or of the next paragraph after it (body-level
/// markers between paragraphs).
fn block_anchor_near(doc: &WordDoc, anchors: &Anchors, part: PartIdx, n: NodeId) -> Option<String> {
    let d = doc.part(part);
    if let Some(p) = d.ancestors(n).find(|&a| d.is(a, ns::W, "p")) {
        return anchors.handle_of(Loc::new(part, p)).map(str::to_owned);
    }
    let mut cur = Some(n);
    while let Some(c) = cur {
        let mut s = d.next_sibling(c);
        while let Some(x) = s {
            if d.is(x, ns::W, "p") {
                return anchors.handle_of(Loc::new(part, x)).map(str::to_owned);
            }
            if let Some(p) = d.descendants_named(x, ns::W, "p").next() {
                return anchors.handle_of(Loc::new(part, p)).map(str::to_owned);
            }
            s = d.next_sibling(x);
        }
        cur = d.parent(c);
    }
    None
}

pub(crate) struct DocCtx<'a> {
    pub doc: &'a WordDoc,
    pub anchors: &'a Anchors,
    pub sheet: &'a StyleSheet,
    pub opts: &'a ProjectOptions,
    pub view: View,
    idx: Cow<'a, DocIndex>,
    /// Revision id → change set.
    rev_cs: HashMap<i64, &'a ChangeSetInfo>,
    /// Flags met while rendering (`hidden_text`, `tiny_text`, `white_text`).
    pub flags: RefCell<BTreeSet<&'static str>>,
    para_cache: RefCell<HashMap<Loc, Rc<EffectiveParagraph>>>,
    base_cache: RefCell<HashMap<Loc, Rc<EffectiveRun>>>,
}

impl std::ops::Deref for DocCtx<'_> {
    type Target = DocIndex;
    fn deref(&self) -> &DocIndex {
        &self.idx
    }
}

impl<'a> DocCtx<'a> {
    /// A context over `idx` when given (it must be of this very document version), else a
    /// freshly built index.
    pub fn with(
        doc: &'a WordDoc,
        anchors: &'a Anchors,
        sheet: &'a StyleSheet,
        opts: &'a ProjectOptions,
        view: View,
        idx: Option<&'a DocIndex>,
    ) -> Self {
        let idx = match idx {
            Some(i) => Cow::Borrowed(i),
            None => Cow::Owned(DocIndex::build(doc, anchors)),
        };
        Self::with_index(doc, anchors, sheet, opts, view, idx)
    }

    fn with_index(
        doc: &'a WordDoc,
        anchors: &'a Anchors,
        sheet: &'a StyleSheet,
        opts: &'a ProjectOptions,
        view: View,
        idx: Cow<'a, DocIndex>,
    ) -> Self {
        let mut rev_cs = HashMap::new();
        for cs in &opts.change_sets {
            for &r in &cs.revisions {
                rev_cs.insert(r, cs);
            }
        }
        DocCtx {
            doc,
            anchors,
            sheet,
            opts,
            view,
            idx,
            rev_cs,
            flags: RefCell::new(BTreeSet::new()),
            para_cache: RefCell::new(HashMap::new()),
            base_cache: RefCell::new(HashMap::new()),
        }
    }

    // ------------------------------------------------------------------ lookups

    pub fn label(&self, loc: Loc) -> Option<&ListLabel> {
        self.labels.get(&loc)
    }

    pub fn comment(&self, wid: i64) -> Option<&CommentInfo> {
        self.comment_by_wid.get(&wid).map(|&i| &self.comments[i])
    }

    /// Does the comment's marker go at its range end (true) or at its reference (false)?
    pub fn has_range_end(&self, wid: i64) -> bool {
        self.range_ended.contains(&wid)
    }

    pub fn note(&self, endnote: bool, id: i64) -> Option<&str> {
        self.notes.get(&(endnote, id)).map(String::as_str)
    }

    pub fn move_source(&self, loc: Loc) -> Option<&str> {
        self.moves.get(&loc).map(String::as_str)
    }

    pub fn change_set(&self, rev: i64) -> Option<&'a ChangeSetInfo> {
        self.rev_cs.get(&rev).copied()
    }

    pub fn is_agent(&self, author: &str) -> bool {
        author == self.opts.agent_author
    }

    /// `AI` for the agent author, else the display name.
    pub fn author_label(&self, author: &str) -> String {
        if self.is_agent(author) {
            "AI".to_owned()
        } else if author.trim().is_empty() {
            "?".to_owned()
        } else {
            author.to_owned()
        }
    }

    /// Is a tracked change by `author` with id `rev` shown with markup in this view?
    pub fn shows(&self, author: &str, rev: Option<i64>) -> bool {
        match self.view {
            View::Markup => true,
            View::Accepted => {
                self.is_agent(author) && rev.and_then(|r| self.change_set(r)).is_none_or(|cs| cs.is_open())
            }
        }
    }

    pub fn story_id(&self, si: usize) -> String {
        self.anchors.story_id(si).unwrap_or("?").to_owned()
    }

    pub fn flag(&self, f: &'static str) {
        self.flags.borrow_mut().insert(f);
    }

    /// Effective paragraph properties (cached).
    pub fn para(&self, part: PartIdx, p: NodeId) -> Rc<EffectiveParagraph> {
        let loc = Loc::new(part, p);
        if let Some(e) = self.para_cache.borrow().get(&loc) {
            return e.clone();
        }
        let e = Rc::new(self.sheet.resolve_paragraph_at(self.doc.part(part), p));
        self.para_cache.borrow_mut().insert(loc, e.clone());
        e
    }

    /// The run formatting of the paragraph's style: what direct formatting is shown relative to.
    pub fn base_run(&self, part: PartIdx, p: NodeId) -> Rc<EffectiveRun> {
        let loc = Loc::new(part, p);
        if let Some(e) = self.base_cache.borrow().get(&loc) {
            return e.clone();
        }
        let e = Rc::new(self.sheet.resolve_style_run(&self.para(part, p)));
        self.base_cache.borrow_mut().insert(loc, e.clone());
        e
    }

    /// Effective run properties.
    pub fn run(&self, part: PartIdx, p: NodeId, r: NodeId) -> EffectiveRun {
        self.sheet.resolve_run(self.doc.part(part), r, &self.para(part, p))
    }

    /// Plain accepted-mode text of a paragraph.
    pub fn accepted_text(&self, part: PartIdx, p: Paragraph) -> String {
        p.text(self.doc.part(part), TextMode::AcceptAll)
    }

    pub fn story(&self, si: usize) -> &'a Story {
        &self.doc.stories()[si]
    }

    /// Paragraph background under a run: cell shading, paragraph shading, page background.
    pub fn para_background(&self, part: PartIdx, p: NodeId) -> Option<String> {
        let ep = self.para(part, p);
        if let Some(f) = ep.shading.as_ref().and_then(|s| s.fill.clone()).filter(|f| f != "auto") {
            return Some(f);
        }
        let d = self.doc.part(part);
        if let Some(tc) = view::Paragraph(p).cell(d) {
            let ctx = crate::wml::style::TableCtx::of(self.sheet, d, tc.0);
            let c = self.sheet.resolve_cell(d, tc.0, &ctx);
            if let Some(f) = c.shading.and_then(|s| s.fill).filter(|f| f != "auto") {
                return Some(f);
            }
        }
        self.background.clone()
    }
}

/// Is `w:settings/w:trackRevisions` on?
pub(crate) fn track_revisions(doc: &WordDoc) -> bool {
    let Some(sp) = doc.settings_part() else { return false };
    let d = doc.part(sp);
    child_on(d, d.root(), "trackRevisions")
}

/// Is document protection enforced (`w:documentProtection/@w:enforcement`)?
pub(crate) fn protected(doc: &WordDoc) -> bool {
    let Some(sp) = doc.settings_part() else { return false };
    let d = doc.part(sp);
    d.find_child(d.root(), ns::W, "documentProtection")
        .and_then(|n| w_attr(d, n, "enforcement"))
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "on"))
}

/// CIE76 colour distance of two `RRGGBB` colours.
pub(crate) fn delta_e(a: &str, b: &str) -> Option<f64> {
    let (l1, a1, b1) = lab(a)?;
    let (l2, a2, b2) = lab(b)?;
    Some(((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt())
}

fn lab(hex: &str) -> Option<(f64, f64, f64)> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let ch = |i: usize| {
        u8::from_str_radix(&hex[i..i + 2], 16).ok().map(|v| {
            let c = f64::from(v) / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        })
    };
    let (r, g, b) = (ch(0)?, ch(2)?, ch(4)?);
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f64| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    Some((116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)))
}

/// The `w:highlight` names as `RRGGBB`.
pub(crate) fn highlight_rgb(name: &str) -> Option<&'static str> {
    Some(match name {
        "black" => "000000",
        "blue" => "0000FF",
        "cyan" => "00FFFF",
        "green" => "00FF00",
        "magenta" => "FF00FF",
        "red" => "FF0000",
        "yellow" => "FFFF00",
        "white" => "FFFFFF",
        "darkBlue" => "000080",
        "darkCyan" => "008080",
        "darkGreen" => "008000",
        "darkMagenta" => "800080",
        "darkRed" => "800000",
        "darkYellow" => "808000",
        "darkGray" => "808080",
        "lightGray" => "C0C0C0",
        _ => return None,
    })
}
