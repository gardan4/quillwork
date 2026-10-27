//! The minimal projection formatter op results and errors carry (spec/projection.md §3, §5, §9).
//!
//! This is deliberately small: block lines with heading kind and list label, CriticMarkup for
//! insertions / deletions / paragraph marks with a `⟨r… author⟩` tag per same-author group,
//! comment markers, compact table rows. Field and control markup, `{==…==}` formatting tags,
//! `|` escaping and the expanded table form are left to the projection module.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::anchors::{Anchors, Target};
use crate::wml::numbering::Numbering;
use crate::wml::story::StoryKind;
use crate::wml::text::{TextMode, text};
use crate::wml::view::{Paragraph, RevisionKind, Table};
use crate::wml::{Loc, PartIdx, WordDoc};
use crate::xml::{Document, NodeId, ns};

/// Which revisions to show.
#[derive(Debug, Clone, Copy)]
pub struct RenderOpts<'a> {
    /// `None` = markup mode (every revision shown). `Some(ids)` = accepted mode: these revision
    /// ids are shown with markup, every other revision is applied silently.
    pub show: Option<&'a HashSet<u32>>,
    /// The engine's agent author (rendered as `AI`).
    pub agent: &'a str,
}

/// Markup-mode lines of `handles`, joined with `\n`.
pub fn render_changed(doc: &WordDoc, anchors: &Anchors, handles: &[String]) -> String {
    render_lines(doc, anchors, handles, &RenderOpts { show: None, agent: "" }).join("\n")
}

/// Projection lines of `handles` (blocks, tables, rows, cells, comments).
pub fn render_lines(doc: &WordDoc, anchors: &Anchors, handles: &[String], opts: &RenderOpts) -> Vec<String> {
    let mut r = Renderer::new(doc, anchors, *opts);
    let mut out = Vec::new();
    for h in handles {
        r.handle(h, &mut out);
    }
    out
}

struct Renderer<'a> {
    doc: &'a WordDoc,
    anchors: &'a Anchors,
    opts: RenderOpts<'a>,
    numbering: Option<Numbering>,
    labels: HashMap<usize, HashMap<NodeId, (String, usize)>>,
    sheet: Option<Option<crate::wml::style::StyleSheet>>,
    comment_ids: Option<HashMap<i64, String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    Ins,
    Del,
}

struct Piece {
    kind: Kind,
    text: String,
    rev: Option<(u32, String)>,
}

impl<'a> Renderer<'a> {
    fn new(doc: &'a WordDoc, anchors: &'a Anchors, opts: RenderOpts<'a>) -> Renderer<'a> {
        Renderer { doc, anchors, opts, numbering: None, labels: HashMap::new(), sheet: None, comment_ids: None }
    }

    fn handle(&mut self, h: &str, out: &mut Vec<String>) {
        let Some(target) = self.anchors.resolve(self.doc, h) else { return };
        match target {
            Target::Block { story, para } => {
                let line = self.block_line(h, story, para);
                out.push(line);
            }
            Target::Table { table } => self.table_lines(h, table, out),
            Target::Row { table, row } => {
                let d = self.doc.part(table.part);
                let line = self.row_line(d, h, row.node, table.node);
                out.push(line);
            }
            Target::Cell { table, cell, .. } => {
                let d = self.doc.part(table.part);
                let t = self.cell_text(d, cell.node);
                out.push(format!("[{h}] {t}").trim_end().to_owned());
            }
            Target::Comment { comment, .. } => {
                let d = self.doc.part(comment.part);
                let paras = super::paras_in(d, comment.node);
                let t: Vec<String> = paras.iter().map(|&p| self.para_text(d, p)).collect();
                out.push(format!("[{h}] {}", t.join(" / ")).trim_end().to_owned());
            }
            Target::Image { .. } => out.push(format!("[{h} IMAGE]")),
        }
    }

    fn block_line(&mut self, h: &str, story: usize, para: Loc) -> String {
        let d = self.doc.part(para.part);
        let mut head = h.to_owned();
        if let Some(kind) = self.kind(d, para.node) {
            head.push(' ');
            head.push_str(&kind);
        }
        if let Some((label, lvl)) = self.label(story, para.node) {
            head.push_str(" #");
            head.push_str(&label);
            if lvl > 0 {
                head.push_str(&format!(" L{lvl}"));
            }
        }
        let body = self.para_text(d, para.node);
        if body.is_empty() { format!("[{head}]") } else { format!("[{head}] {body}") }
    }

    fn kind(&mut self, d: &Document, p: NodeId) -> Option<String> {
        let id = Paragraph(p).style_id(d)?;
        if self.sheet.is_none() {
            self.sheet = Some(self.doc.style_sheet().ok().flatten());
        }
        let sheet = self.sheet.as_ref().and_then(Option::as_ref);
        let Some(sheet) = sheet else { return Some(format!("STYLE={id}")) };
        if sheet.default_paragraph_style_id() == Some(id.as_str()) {
            return None;
        }
        let style = sheet.style_by_id(&id);
        let name = style.and_then(|s| s.name()).unwrap_or("").to_ascii_lowercase();
        if id == "Title" || name == "title" {
            return Some("TITLE".to_owned());
        }
        if let Some(l) = style.and_then(|s| s.outline_level())
            && (0..9).contains(&l)
        {
            return Some(format!("H{}", l + 1));
        }
        if let Some(n) = name.strip_prefix("heading ").and_then(|n| n.trim().parse::<u32>().ok()) {
            return Some(format!("H{n}"));
        }
        Some(format!("STYLE={id}"))
    }

    fn label(&mut self, story: usize, p: NodeId) -> Option<(String, usize)> {
        if !self.labels.contains_key(&story) {
            let numbering = self.numbering.get_or_insert_with(|| Numbering::load(self.doc));
            let mut map = HashMap::new();
            if let Some(st) = self.doc.stories().get(story) {
                for (para, label) in numbering.labels(self.doc, st) {
                    if let Some(l) = label {
                        map.insert(para.node(), (l.text, l.ilvl));
                    }
                }
            }
            self.labels.insert(story, map);
        }
        self.labels.get(&story).and_then(|m| m.get(&p)).cloned()
    }

    fn comment_handle(&mut self, id: i64) -> Option<String> {
        if self.comment_ids.is_none() {
            let mut m = HashMap::new();
            for (i, st) in self.doc.stories().iter().enumerate() {
                if let StoryKind::Comment { id } = st.kind
                    && let Some(s) = self.anchors.story_id(i)
                {
                    m.insert(id, s.to_owned());
                }
            }
            self.comment_ids = Some(m);
        }
        self.comment_ids.as_ref().and_then(|m| m.get(&id)).cloned()
    }

    fn shown(&self, id: u32) -> bool {
        self.opts.show.is_none_or(|s| s.contains(&id))
    }

    fn author_label(&self, author: &str) -> String {
        if !self.opts.agent.is_empty() && author == self.opts.agent { "AI".to_owned() } else { author.to_owned() }
    }

    /// The paragraph's text with markup.
    fn para_text(&mut self, d: &Document, p: NodeId) -> String {
        let mut pieces: Vec<Piece> = Vec::new();
        let mut comments: Vec<(usize, i64)> = Vec::new();
        crate::edit::walk(d, p, |c| {
            if crate::edit::is_ignored_branch(d, c) {
                return false;
            }
            let local = d.local_name(c).unwrap_or("");
            let is_w = d.namespace(c) == Some(ns::W);
            let is_math_t = d.namespace(c) == Some(ns::M) && local == "t";
            if is_w {
                if local.ends_with("Pr") || local.ends_with("PrChange") {
                    return false;
                }
                match local {
                    "instrText" | "delInstrText" | "txbxContent" | "drawing" | "pict" | "object" | "p" => return false,
                    "commentRangeEnd" => {
                        if let Some(id) = attr_int(d, c) {
                            comments.push((pieces.len(), id));
                        }
                        return false;
                    }
                    "commentReference" => {
                        if let Some(id) = attr_int(d, c)
                            && !has_range_end(d, id)
                        {
                            comments.push((pieces.len(), id));
                        }
                        return false;
                    }
                    _ => {}
                }
            }
            let leaf = is_math_t
                || (is_w
                    && matches!(
                        local,
                        "t" | "delText" | "tab" | "ptab" | "br" | "cr" | "noBreakHyphen" | "softHyphen" | "sym"
                    ));
            if !leaf {
                return true;
            }
            let s = text(d, c, TextMode::AsIs);
            if s.is_empty() {
                return false;
            }
            let (kind, rev) = state(d, p, c);
            let s: String = s
                .chars()
                .map(|ch| match ch {
                    '\t' => '⇥',
                    '\n' => '↵',
                    ch => ch,
                })
                .collect();
            pieces.push(Piece { kind, text: s, rev });
            false
        });
        // Accepted mode: other people's revisions applied silently.
        let mut marks: Vec<(usize, String)> = Vec::new();
        for (at, id) in comments {
            if let Some(h) = self.comment_handle(id) {
                marks.push((at, format!("{{>>{h}<<}}")));
            }
        }
        let mut list: Vec<Piece> = Vec::new();
        let mut mi = 0;
        for (i, mut piece) in pieces.into_iter().enumerate() {
            while mi < marks.len() && marks[mi].0 == i {
                list.push(Piece { kind: Kind::Plain, text: std::mem::take(&mut marks[mi].1), rev: None });
                mi += 1;
            }
            if let Some((id, _)) = &piece.rev
                && !self.shown(*id)
            {
                match piece.kind {
                    Kind::Ins => {
                        piece.kind = Kind::Plain;
                        piece.rev = None;
                    }
                    Kind::Del => continue,
                    Kind::Plain => {}
                }
            }
            list.push(piece);
        }
        while mi < marks.len() {
            list.push(Piece { kind: Kind::Plain, text: std::mem::take(&mut marks[mi].1), rev: None });
            mi += 1;
        }
        // Paragraph mark.
        if let Some(rev) = Paragraph(p).mark_revision(d) {
            let kind = match rev.kind {
                RevisionKind::ParaMarkIns => Some(Kind::Ins),
                RevisionKind::ParaMarkDel => Some(Kind::Del),
                _ => None,
            };
            if let Some(kind) = kind {
                let id = rev.id(d).unwrap_or(0).max(0) as u32;
                if self.shown(id) {
                    let author = crate::track::write::w_attr(d, rev.node, "author").unwrap_or_default();
                    list.push(Piece { kind, text: "¶".to_owned(), rev: Some((id, author)) });
                }
            }
        }
        self.join(list)
    }

    fn join(&self, list: Vec<Piece>) -> String {
        let mut out = String::new();
        let mut i = 0;
        while i < list.len() {
            if list[i].kind == Kind::Plain {
                out.push_str(&list[i].text);
                i += 1;
                continue;
            }
            // A group: consecutive revision pieces by one author.
            let author = list[i].rev.as_ref().map(|r| r.1.clone()).unwrap_or_default();
            let mut ids: Vec<u32> = Vec::new();
            let mut j = i;
            while j < list.len()
                && list[j].kind != Kind::Plain
                && list[j].rev.as_ref().map(|r| r.1.as_str()).unwrap_or("") == author
            {
                let kind = list[j].kind;
                let mut chunk = String::new();
                while j < list.len()
                    && list[j].kind == kind
                    && list[j].rev.as_ref().map(|r| r.1.as_str()).unwrap_or("") == author
                {
                    chunk.push_str(&list[j].text);
                    if let Some((id, _)) = list[j].rev {
                        ids.push(id);
                    }
                    j += 1;
                }
                let (open, close) = if kind == Kind::Ins { ("{++", "++}") } else { ("{--", "--}") };
                out.push_str(open);
                out.push_str(&chunk);
                out.push_str(close);
            }
            ids.sort_unstable();
            ids.dedup();
            let ids: Vec<String> = ids.iter().map(u32::to_string).collect();
            out.push_str(&format!("⟨r{} {}⟩", ids.join(","), self.author_label(&author)));
            i = j;
        }
        out
    }

    fn table_lines(&mut self, h: &str, table: Loc, out: &mut Vec<String>) {
        let d = self.doc.part(table.part);
        let rows = Table(table.node).rows(d);
        let cols = grid_cols(d, table.node).unwrap_or_else(|| rows.iter().map(|r| r.cells(d).len()).max().unwrap_or(0));
        let caption = d
            .find_child(table.node, ns::W, "tblPr")
            .and_then(|pr| d.find_child(pr, ns::W, "tblCaption"))
            .and_then(|c| crate::track::write::w_attr(d, c, "val"));
        let mut head = format!("[{h} TABLE {}×{cols}", rows.len());
        if let Some(c) = caption {
            head.push_str(&format!(" \"{c}\""));
        }
        head.push(']');
        out.push(head);
        for (i, row) in rows.iter().enumerate() {
            let line = self.row_line(d, &format!("{h}.r{}", i + 1), row.0, table.node);
            out.push(format!("  {line}"));
        }
    }

    fn row_line(&mut self, d: &Document, h: &str, tr: NodeId, _tbl: NodeId) -> String {
        let cells = crate::wml::view::Row(tr).cells(d);
        let texts: Vec<String> = cells.iter().map(|c| self.cell_text(d, c.0)).collect();
        let hdr =
            d.find_child(tr, ns::W, "trPr").and_then(|pr| d.find_child(pr, ns::W, "tblHeader")).is_some_and(|n| {
                !matches!(crate::track::write::w_attr(d, n, "val").as_deref(), Some("0" | "false" | "off"))
            });
        let head = if hdr { format!("{h} hdr") } else { h.to_owned() };
        let body = texts.join(" | ");
        if body.trim().is_empty() { format!("[{head}]") } else { format!("[{head}] {body}") }
    }

    fn cell_text(&mut self, d: &Document, tc: NodeId) -> String {
        let paras = super::paras_in(d, tc);
        let t: Vec<String> = paras.iter().map(|&p| self.para_text(d, p)).collect();
        t.join(" / ")
    }
}

fn attr_int(d: &Document, n: NodeId) -> Option<i64> {
    crate::track::write::w_attr(d, n, "id").and_then(|v| v.trim().parse().ok())
}

fn has_range_end(d: &Document, id: i64) -> bool {
    d.descendants(d.root()).any(|n| d.is(n, ns::W, "commentRangeEnd") && attr_int(d, n) == Some(id))
}

/// Innermost `w:ins` / `w:del` / `w:moveTo` / `w:moveFrom` around `c` (below `p`).
fn state(d: &Document, p: NodeId, c: NodeId) -> (Kind, Option<(u32, String)>) {
    for a in d.ancestors(c) {
        if a == p {
            break;
        }
        if d.namespace(a) != Some(ns::W) {
            continue;
        }
        let kind = match d.local_name(a) {
            Some("ins" | "moveTo") => Kind::Ins,
            Some("del" | "moveFrom") => Kind::Del,
            _ => continue,
        };
        let id = attr_int(d, a).unwrap_or(0).max(0) as u32;
        let author = crate::track::write::w_attr(d, a, "author").unwrap_or_default();
        return (kind, Some((id, author)));
    }
    (Kind::Plain, None)
}

fn grid_cols(d: &Document, tbl: NodeId) -> Option<usize> {
    let g = d.find_child(tbl, ns::W, "tblGrid")?;
    let n = d.child_elements(g).filter(|&c| d.is(c, ns::W, "gridCol")).count();
    (n > 0).then_some(n)
}

/// Accepted-mode text of `p` (what `expect` compares).
pub fn accepted_text(d: &Document, p: NodeId) -> String {
    text(d, p, TextMode::AcceptAll)
}

/// The part a story lives in (helper for callers rendering whole stories).
pub fn story_part(doc: &WordDoc, story: usize) -> Option<PartIdx> {
    doc.stories().get(story).map(|s| s.part)
}

// ---------------------------------------------------------------------- §9 text forms

fn str_list(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

/// The text form of an OpResult JSON (spec/projection.md §9).
pub fn render_result_text(result: &Value, review: bool) -> String {
    let version = result.get("version").and_then(Value::as_u64).unwrap_or(0);
    let changed = str_list(&result["changed"]);
    let cs = match result.get("change_set").and_then(Value::as_str) {
        Some(cs) => cs.to_owned(),
        None if review => "review".to_owned(),
        None => "direct".to_owned(),
    };
    let mut out = vec![format!("§ v{version} · {cs} · {} changed", changed.len())];
    out.extend(changed);
    let removed = str_list(&result["removed"]);
    if !removed.is_empty() {
        out.push(format!("§ removed: {}", removed.join(", ")));
    }
    // `outline` already holds the whole line; an older host may have stripped the prefix.
    if let Some(o) = result.get("outline").and_then(Value::as_str) {
        out.push(if o.starts_with("§ outline: ") { o.to_owned() } else { format!("§ outline: {o}") });
    }
    for f in result.get("lint").and_then(Value::as_array).into_iter().flatten() {
        let s = |k: &str| f.get(k).and_then(Value::as_str).unwrap_or("");
        out.push(format!("§ lint: {} {} {} — {}", s("severity"), s("rule"), s("anchor"), s("message")));
    }
    for w in str_list(&result["warnings"]) {
        out.push(format!("§ warning: {w}"));
    }
    out.join("\n")
}

/// The text form of an error envelope JSON (spec/projection.md §9).
pub fn render_error_text(envelope: &Value) -> String {
    let e = &envelope["error"];
    let code = e.get("code").and_then(Value::as_str).unwrap_or("INTERNAL");
    let mut head = format!("§ error {code}");
    if let Some(v) = e.get("version").and_then(Value::as_u64) {
        head.push_str(&format!(" · v{v}"));
    }
    if let Some(i) = e.get("op_index").and_then(Value::as_u64) {
        let op = e.get("op").and_then(Value::as_str).unwrap_or("");
        head.push_str(&format!(" · ops[{i}] {op}"));
    }
    if let Some(a) = e.get("anchor").and_then(Value::as_str) {
        head.push_str(&format!(" · {a}"));
    }
    let retry = e.get("retryable").and_then(Value::as_bool).unwrap_or(false);
    head.push_str(if retry { " · retryable" } else { " · final" });
    let mut out = vec![head, format!("§ {}", e.get("message").and_then(Value::as_str).unwrap_or(""))];
    out.extend(str_list(&e["current"]));
    if let Some(cands) = e.get("candidates").and_then(Value::as_array) {
        for c in cands {
            let a = c.get("anchor").and_then(Value::as_str).unwrap_or("");
            let ctx = c.get("context").and_then(Value::as_str).unwrap_or("");
            match c.get("occurrence").and_then(Value::as_u64) {
                Some(o) => out.push(format!("  {a} #{o}: {ctx}")),
                None => out.push(format!("  {a}: {ctx}")),
            }
        }
    }
    if let Some(u) = e.get("usage").and_then(Value::as_str) {
        out.push(format!("§ usage: {u}"));
    }
    out.join("\n")
}
