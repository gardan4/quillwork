//! The read ops past `outline` / `read` / `find` (spec/projection.md §8): `get_block`,
//! `list_changes`, `list_comments`, `list_images`, `get_image`, `lint`, `list_fields` and
//! `help`. The structured ones answer in `data`, `lint` and `help` in `text`.

// `OpError` is large (see `ops`); it is built once per failed read, so boxing buys nothing.
#![allow(clippy::result_large_err)]

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value, json};

use super::{ChangeSetRec, Engine, READ_USAGE};
use crate::anchors::Target;
use crate::ops::{self, OpError, handlers};
use crate::project::vm::{Block, ImageBase, Inline, Paragraph, Revision, ViewModel};
use crate::project::{self, Fields};
use crate::validate::{self, Severity};
use crate::wml::{Loc, StoryKind};
use crate::xml::ns;

/// What a read op answers with.
pub(super) enum Answer {
    Text(String),
    Data(Value),
}

/// Most findings `lint` lists before it cuts the list.
pub(super) const LINT_MAX: usize = 200;
/// Characters of revision text `list_changes` shows.
const REV_TEXT: usize = 200;
/// `LIMIT image_bytes`: the largest image `get_image` returns.
pub(super) const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// One `lint` finding (spec/schema/common.schema.json `LintFinding`, plus the rules
/// spec/projection.md §8 adds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Finding {
    pub rule: &'static str,
    pub severity: &'static str,
    /// `-` when the finding has no block (a package-level validator finding).
    pub anchor: String,
    pub message: String,
}

impl Finding {
    fn new(rule: &'static str, severity: &'static str, anchor: &str, message: String) -> Finding {
        Finding { rule, severity, anchor: anchor.to_owned(), message }
    }

    pub fn line(&self) -> String {
        format!("§ lint: {} {} {} — {}", self.severity, self.rule, self.anchor, self.message)
    }

    pub fn to_json(&self) -> Value {
        json!({ "rule": self.rule, "severity": self.severity, "anchor": self.anchor, "message": self.message })
    }
}

/// The usage line of a read op.
fn usage(op: &str) -> String {
    READ_USAGE.iter().find(|(n, _)| *n == op).map_or_else(|| format!("{op} {{…}}"), |(_, u)| (*u).to_owned())
}

fn bad(op: &str, path: &str, msg: impl Into<String>) -> OpError {
    OpError { usage: Some(usage(op)), ..OpError::invalid(path, msg) }
}

fn anchor_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"^(b[0-9a-hjkmnp-tv-z]{3,}|(h|f|fn|en|c|tb)[1-9][0-9]*-[0-9a-hjkmnp-tv-z]+|t[1-9][0-9]*(\.r[1-9][0-9]*(\.c[1-9][0-9]*)?)?|img[1-9][0-9]*|c[1-9][0-9]*)$",
        )
        .expect("anchor regex")
    })
}

fn is_image_anchor(s: &str) -> bool {
    s.strip_prefix("img").is_some_and(|n| !n.is_empty() && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit()))
}

fn is_asset_id(s: &str) -> bool {
    s.len() == 18 && s.starts_with("a_") && s[2..].bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_change_set_id(s: &str) -> bool {
    s.strip_prefix("cs_")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()))
}

// ------------------------------------------------------------------------------ walking

/// Story ids in document order: the body, then headers, footers, footnotes, endnotes and text
/// boxes, each by number.
fn stories(vm: &ViewModel) -> Vec<&str> {
    let rank = |id: &str| {
        let (kind, n) = id.split_at(id.find(|c: char| c.is_ascii_digit()).unwrap_or(id.len()));
        let k = ["h", "f", "fn", "en", "tb"].iter().position(|x| *x == kind).unwrap_or(9);
        (k, n.parse::<u64>().unwrap_or(0))
    };
    let mut ids: Vec<&str> = vm.stories.keys().map(String::as_str).collect();
    ids.sort_by_key(|id| rank(id));
    ids.insert(0, "b");
    ids
}

/// Every block of every story (not comments), tables' nested blocks after the table, with its
/// story id and the cell it sits in.
fn each_block<'a>(vm: &'a ViewModel, f: &mut dyn FnMut(&'a str, Option<&'a str>, &'a Block)) {
    fn rec<'a>(
        story: &'a str,
        cell: Option<&'a str>,
        blocks: &'a [Block],
        f: &mut dyn FnMut(&'a str, Option<&'a str>, &'a Block),
    ) {
        for b in blocks {
            f(story, cell, b);
            if let Block::Table(t) = b {
                for r in &t.rows {
                    for c in &r.cells {
                        rec(story, Some(&c.id), &c.blocks, f);
                    }
                }
            }
        }
    }
    for s in stories(vm) {
        if let Some(blocks) = vm.story_blocks(s) {
            rec(s, None, blocks, f);
        }
    }
}

/// A paragraph's text in the accepted view.
fn para_text(inlines: &[Inline]) -> String {
    inlines.iter().map(Inline::accepted_text).collect()
}

/// The accepted text of blocks (paragraphs one per line, table cells by tab).
fn blocks_text(blocks: &[Block]) -> String {
    let mut lines = Vec::new();
    for b in blocks {
        match b {
            Block::P(p) => lines.push(para_text(&p.inlines)),
            Block::Table(t) => {
                for r in &t.rows {
                    let cells: Vec<String> =
                        r.cells.iter().map(|c| blocks_text(&c.blocks).replace('\n', " ")).collect();
                    lines.push(cells.join("\t"));
                }
            }
            Block::Image(_) => {}
        }
    }
    lines.join("\n")
}

/// Position of every anchor in document order (for sorting findings).
fn positions(vm: &ViewModel) -> HashMap<String, usize> {
    let mut out = Vec::new();
    for s in stories(vm) {
        for b in vm.story_blocks(s).map(Vec::as_slice).unwrap_or_default() {
            b.anchors(&mut out);
        }
    }
    for c in &vm.comments {
        out.push(c.id.clone());
    }
    let mut m = HashMap::with_capacity(out.len());
    for (i, a) in out.into_iter().enumerate() {
        m.entry(a).or_insert(i);
    }
    m
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

// ------------------------------------------------------------------------------ the ops

impl Engine {
    /// Serve one of the read ops of this module; `obj` has passed the key check.
    pub(super) fn read_more(&self, op: &str, obj: &Map<String, Value>) -> Result<Answer, OpError> {
        match op {
            "get_block" => self.get_block(obj),
            "list_changes" => self.list_changes(obj),
            "list_comments" => self.list_comments(obj),
            "list_images" => self.list_images(),
            "get_image" => self.get_image(obj),
            "lint" => self.lint_op(),
            "list_fields" => self.list_fields(),
            "help" => help(obj),
            _ => Err(bad("read", "/op", format!("{op:?} is not a read op"))),
        }
    }

    fn vm_or_fail(&self) -> Result<std::sync::Arc<ViewModel>, OpError> {
        self.vm_now().map_err(|e| OpError::new("INTERNAL", e.to_string()))
    }

    fn get_block(&self, obj: &Map<String, Value>) -> Result<Answer, OpError> {
        let anchor = match obj.get("anchor") {
            Some(Value::String(a)) if anchor_re().is_match(a) => a.as_str(),
            Some(Value::String(a)) => {
                return Err(bad(
                    "get_block",
                    "/anchor",
                    format!("{a:?} is not an anchor (b105, t1.r2.c3, img1, c1, h1-1x2, …)"),
                ));
            }
            Some(_) => {
                return Err(bad("get_block", "/anchor", "anchor is an anchor string (b105, t1.r2.c3, img1, c1, …)"));
            }
            None => return Err(bad("get_block", "/anchor", "anchor (the block to show) is required")),
        };
        let vm = self.vm_or_fail()?;
        if let Some(c) = vm.comments.iter().find(|c| c.id == anchor) {
            return Ok(Answer::Data(json!({ "comment": c })));
        }
        let mut hit: Option<Value> = None;
        let look = |story: &str, blocks: &[Block], hit: &mut Option<Value>| {
            fn rec(story: &str, cell: Option<&str>, blocks: &[Block], anchor: &str, hit: &mut Option<Value>) {
                for b in blocks {
                    if hit.is_some() {
                        return;
                    }
                    if b.id() == anchor {
                        let mut v = json!({ "story": story, "block": b });
                        if let Some(c) = cell {
                            v["cell"] = json!(c);
                        }
                        *hit = Some(v);
                        return;
                    }
                    match b {
                        Block::P(p) => {
                            for i in &p.inlines {
                                if let Some(img) = i.image.as_deref().filter(|img| img.id == anchor) {
                                    *hit = Some(json!({ "story": story, "para": p.id, "image": img }));
                                    return;
                                }
                            }
                        }
                        Block::Table(t) => {
                            for r in &t.rows {
                                if r.id == anchor {
                                    *hit = Some(json!({ "story": story, "table": t.id, "row": r }));
                                    return;
                                }
                                for c in &r.cells {
                                    if c.id == anchor {
                                        *hit = Some(json!({ "story": story, "table": t.id, "cell": c }));
                                        return;
                                    }
                                    rec(story, Some(&c.id), &c.blocks, anchor, hit);
                                }
                            }
                        }
                        Block::Image(_) => {}
                    }
                }
            }
            rec(story, None, blocks, anchor, hit);
        };
        for s in stories(&vm) {
            if let Some(blocks) = vm.story_blocks(s) {
                look(s, blocks, &mut hit);
            }
        }
        for c in &vm.comments {
            look(&c.id, &c.blocks, &mut hit);
        }
        match hit {
            Some(v) => Ok(Answer::Data(v)),
            None => Err(OpError::not_found("anchor", format!("no block, row, cell, image or comment {anchor}"))),
        }
    }

    fn list_changes(&self, obj: &Map<String, Value>) -> Result<Answer, OpError> {
        let opt_str = |k: &str| -> Result<Option<&str>, OpError> {
            match obj.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.as_str())),
                Some(_) => Err(bad("list_changes", &format!("/{k}"), format!("{k} is a non-empty string"))),
            }
        };
        let cs_filter = opt_str("change_set")?;
        let author = opt_str("author")?;
        let status = opt_str("status")?;
        if let Some(cs) = cs_filter {
            if !is_change_set_id(cs) {
                return Err(bad("list_changes", "/change_set", format!("{cs:?} is not a change-set id (cs_7)")));
            }
            if !self.state.change_sets.iter().any(|c| c.id == cs) {
                return Err(OpError::not_found("change_set", format!("no change set {cs} in this session's log")));
            }
        }
        if let Some(s) = status
            && !matches!(s, "open" | "accepted" | "rejected" | "partial")
        {
            return Err(bad("list_changes", "/status", "status is open, accepted, rejected or partial"));
        }
        let author_is = |a: &str| author.is_none_or(|want| a == want || (want == "AI" && a == self.agent));
        let cs_status: HashMap<&str, &'static str> =
            self.state.change_sets.iter().map(|c| (c.id.as_str(), c.status())).collect();

        let vm = self.vm_or_fail()?;
        let mut revs = Revs::default();
        each_block(&vm, &mut |story, _cell, b| revs.block(story, b));
        for c in &vm.comments {
            for b in &c.blocks {
                revs.block(&c.id, b);
            }
        }
        let revisions: Vec<Value> = revs
            .list
            .into_iter()
            .filter(|r| cs_filter.is_none_or(|cs| r.rev.cs.as_deref() == Some(cs)))
            .filter(|r| author_is(&r.rev.author))
            .filter(|r| {
                status.is_none_or(|s| {
                    let st = r.rev.cs.as_deref().and_then(|cs| cs_status.get(cs).copied()).unwrap_or("open");
                    st == s
                })
            })
            .map(|r| {
                let mut v = json!({
                    "id": r.rev.id,
                    "type": r.rev.typ,
                    "author": r.rev.author,
                    "agent": r.rev.author == self.agent,
                    "date": r.rev.date,
                    "change_set": r.rev.cs,
                    "story": r.story,
                    "anchors": r.anchors,
                    "text": cut(&r.text, REV_TEXT),
                });
                if let Some(old) = &r.rev.old {
                    v["old"] = json!(old);
                }
                v
            })
            .collect();
        let change_sets: Vec<Value> = self
            .state
            .change_sets
            .iter()
            .filter(|c| cs_filter.is_none_or(|cs| c.id == cs))
            .filter(|c| author_is(&c.author))
            .filter(|c| status.is_none_or(|s| c.status() == s))
            .map(ChangeSetRec::to_json)
            .collect();
        Ok(Answer::Data(json!({ "revisions": revisions, "change_sets": change_sets })))
    }

    fn list_comments(&self, obj: &Map<String, Value>) -> Result<Answer, OpError> {
        let open_only = match obj.get("open_only") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(bad("list_comments", "/open_only", "open_only is true or false")),
        };
        let vm = self.vm_or_fail()?;
        let ids: HashSet<&str> = vm.comments.iter().map(|c| c.id.as_str()).collect();
        let is_root = |c: &&project::vm::Comment| c.parent.as_deref().is_none_or(|p| !ids.contains(p));
        let comments: Vec<Value> = vm
            .comments
            .iter()
            .filter(is_root)
            .filter(|c| !(open_only && c.done))
            .map(|c| {
                // Oldest first (an undated reply keeps its place ahead of dated ones).
                let mut replies: Vec<(usize, &project::vm::Comment)> = vm
                    .comments
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.parent.as_deref() == Some(c.id.as_str()))
                    .collect();
                replies.sort_by_key(|(i, r)| (r.date.clone().unwrap_or_default(), *i));
                let replies: Vec<Value> = replies
                    .iter()
                    .map(|(_, r)| json!({ "id": r.id, "author": r.author, "text": blocks_text(&r.blocks) }))
                    .collect();
                json!({
                    "id": c.id,
                    "author": c.author,
                    "done": c.done,
                    "on": c.anchor.as_ref().map_or("", |a| a.text.as_str()),
                    "anchor": c.anchor.as_ref().map(|a| a.start.clone()),
                    "text": blocks_text(&c.blocks),
                    "replies": replies,
                })
            })
            .collect();
        Ok(Answer::Data(json!({ "comments": comments })))
    }

    fn list_images(&self) -> Result<Answer, OpError> {
        let vm = self.vm_or_fail()?;
        let mut images = Vec::new();
        each_image(&vm, &mut |story, para, img, rev| {
            let mut v = serde_json::to_value(img).unwrap_or(Value::Null);
            if let Some(o) = v.as_object_mut() {
                o.insert("story".into(), json!(story));
                o.insert("para".into(), json!(para));
                if let Some(r) = rev {
                    o.insert("rev".into(), json!(r));
                }
                let media = img.asset.as_ref().and_then(|a| vm.assets.get(a));
                o.insert("media".into(), json!(media));
            }
            images.push(v);
        });
        Ok(Answer::Data(json!({ "images": images })))
    }

    fn get_image(&self, obj: &Map<String, Value>) -> Result<Answer, OpError> {
        let image = obj.get("image").filter(|v| !v.is_null());
        let asset = obj.get("asset").filter(|v| !v.is_null());
        let (image, asset_id) = match (image, asset) {
            (Some(_), Some(_)) => return Err(bad("get_image", "/asset", "give image or asset, not both")),
            (None, None) => return Err(bad("get_image", "/image", "image (img3) or asset (a_…) is required")),
            (Some(i), None) => {
                let Some(i) = i.as_str().filter(|i| is_image_anchor(i)) else {
                    return Err(bad("get_image", "/image", "image is an image anchor (img3)"));
                };
                let vm = self.vm_or_fail()?;
                let mut found: Option<ImageBase> = None;
                each_image(&vm, &mut |_, _, img, _| {
                    if found.is_none() && img.id == i {
                        found = Some(img.clone());
                    }
                });
                let Some(img) = found else {
                    return Err(OpError::not_found("anchor", format!("no image {i}")));
                };
                if img.external {
                    return Err(OpError::unsupported(
                        "unsupported_target",
                        format!("{i} is a linked picture; its bytes are not in the document"),
                    ));
                }
                let Some(a) = img.asset else {
                    return Err(OpError::unsupported(
                        "unsupported_target",
                        format!("{i} has no picture (a shape, chart or text box)"),
                    ));
                };
                (Some(i.to_owned()), a)
            }
            (None, Some(a)) => match a.as_str().filter(|a| is_asset_id(a)) {
                Some(a) => (None, a.to_owned()),
                None => return Err(bad("get_image", "/asset", "asset is an asset id (a_ + 16 hex digits)")),
            },
        };
        let (mime, bytes) = project::asset_bytes(&self.doc, &asset_id)
            .map_err(|_| OpError::not_found("asset", format!("no asset {asset_id} in the document")))?;
        if bytes.len() > MAX_IMAGE_BYTES {
            let o = OpError {
                limit: Some(ops::LimitInfo {
                    name: "image_bytes".into(),
                    max: MAX_IMAGE_BYTES as i64,
                    actual: bytes.len() as i64,
                }),
                ..OpError::new(
                    "LIMIT",
                    format!("{asset_id} is {} bytes; get_image returns at most {MAX_IMAGE_BYTES}", bytes.len()),
                )
            };
            return Err(o);
        }
        Ok(Answer::Data(json!({
            "image": image,
            "asset": asset_id,
            "mime": mime,
            "bytes": bytes.len(),
            "px": project::pixel_size(&bytes),
            "needs_conversion": project::needs_conversion(&mime),
            "base64": base64(&bytes),
        })))
    }

    fn list_fields(&self) -> Result<Answer, OpError> {
        let f = self.fields_now()?;
        let controls: Vec<Value> = f
            .controls
            .iter()
            .map(|c| {
                let value = match (c.checked, &c.value) {
                    (Some(on), _) => json!(on),
                    (None, Some(v)) => json!(v),
                    (None, None) => Value::Null,
                };
                json!({
                    "anchor": c.anchor,
                    "tag": c.tag,
                    "alias": c.alias,
                    "type": c.typ,
                    "value": value,
                    "locked": c.locked,
                    "bound": c.bound,
                    "items": c.items,
                })
            })
            .collect();
        let placeholders: Vec<Value> =
            f.placeholders.iter().map(|p| json!({ "anchor": p.anchor, "text": p.text })).collect();
        Ok(Answer::Data(json!({ "controls": controls, "placeholders": placeholders })))
    }

    fn fields_now(&self) -> Result<Fields, OpError> {
        let p = self.projector().map_err(|e| OpError::new("INTERNAL", e.to_string()))?;
        p.fields(&self.doc, &self.anchors, &self.project_options()).map_err(|e| OpError::new("INTERNAL", e.to_string()))
    }

    fn lint_op(&self) -> Result<Answer, OpError> {
        let vm = self.vm_or_fail()?;
        let fields = self.fields_now()?;
        let mut found = self.lint_findings(&vm, &fields);
        let report = validate::check_with(&self.doc, Some(&self.anchors));
        for f in &report.findings {
            let severity = if f.severity == Severity::Error { "warning" } else { "info" };
            let anchor = f.anchor.clone().unwrap_or_else(|| "-".into());
            found.push(Finding::new("invalid_markup", severity, &anchor, format!("{}: {}", f.code, f.message)));
        }
        sort_findings(&mut found, &positions(&vm));
        Ok(Answer::Text(lint_text(&found)))
    }

    /// The lint findings of the current version: every rule but `invalid_markup`.
    pub(super) fn lint_findings(&self, vm: &ViewModel, fields: &Fields) -> Vec<Finding> {
        let mut out = Vec::new();
        // Paragraphs deleted with tracked changes: a reference to one breaks on accept.
        let mut deleted: HashSet<&str> = HashSet::new();
        each_block(vm, &mut |_, _, b| {
            if let Block::P(p) = b
                && gone(p)
            {
                deleted.insert(&p.id);
            }
        });
        for r in &fields.refs {
            out.extend(ref_finding(r, &|t| deleted.contains(t)));
        }
        out.extend(fields.placeholders.iter().map(placeholder_finding));
        let mut last_heading: Option<u8> = None;
        each_block(vm, &mut |story, cell, b| {
            let Block::P(p) = b else { return };
            if story == "b"
                && cell.is_none()
                && let Some(h) = p.heading
            {
                out.extend(heading_finding(&p.id, h, last_heading));
                last_heading = Some(h);
            }
            self.para_findings(p, cell.is_some(), &mut out);
        });
        each_image(vm, &mut |_, _, img, _| out.extend(alt_finding(img)));
        out
    }

    /// `double_space` and `empty_paragraph` on one paragraph. An empty paragraph in a table cell
    /// is the cell's required content (an empty cell, or a new row), never noise.
    fn para_findings(&self, p: &Paragraph, in_cell: bool, out: &mut Vec<Finding>) {
        // An inline picture between two spaces is not a double space.
        let spaced: String =
            p.inlines.iter().map(|i| if i.image.is_some() { "\u{FFFC}" } else { i.accepted_text() }).collect();
        if let Some(m) = double_space_re().find(&spaced) {
            let before: String =
                spaced[..m.start() + 1].chars().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect();
            out.push(Finding::new(
                "double_space",
                "info",
                &p.id,
                format!("double space after \"{}\"", before.trim_start()),
            ));
        }
        let deleted = p.mark.as_ref().is_some_and(|m| m.typ == "del" || m.typ == "moveFrom");
        let empty =
            para_text(&p.inlines).trim().is_empty() && p.inlines.iter().all(|i| i.image.is_none() && i.k != "note");
        if empty && !deleted && !in_cell && self.state.last_changed.contains_key(&p.id) {
            out.push(Finding::new("empty_paragraph", "info", &p.id, "empty paragraph left by an edit".into()));
        }
    }

    /// Op-result lint: findings on the paragraphs a transaction changed (tables and images by
    /// their paragraphs), plus every reference it broke elsewhere: its bookmark is gone, its
    /// paragraph is now deleted with tracked changes, or that paragraph was deleted outright
    /// (the bookmark then moves to a neighbour, so the reference silently points elsewhere).
    ///
    /// It stays within the per-op budget on long documents: the changed paragraphs are projected
    /// on their own, and the whole document is scanned for references only when a bookmark
    /// target was removed, deleted or moved.
    pub(super) fn tx_findings(
        &self,
        changed: &[String],
        removed: &[String],
        bookmarks_before: &HashMap<String, String>,
    ) -> Vec<Finding> {
        let Ok(proj) = self.projector() else { return Vec::new() };
        let opts = self.project_options();
        let (doc, anchors) = (&self.doc, &self.anchors);
        let body = doc.stories().iter().position(|s| matches!(s.kind, StoryKind::Body));
        let mut paras: Vec<(String, Loc)> = Vec::new();
        let mut push = |loc: Loc| {
            if let Some(h) = anchors.handle_of(loc)
                && !paras.iter().any(|(x, _)| x == h)
            {
                paras.push((h.to_owned(), loc));
            }
        };
        for h in changed {
            let within = match anchors.resolve(doc, h) {
                Some(Target::Block { para, .. }) => {
                    push(para);
                    continue;
                }
                Some(Target::Table { table: l })
                | Some(Target::Row { row: l, .. })
                | Some(Target::Cell { cell: l, .. }) => l,
                Some(Target::Image { drawing: l }) => {
                    let d = doc.part(l.part);
                    if let Some(p) = d.ancestors(l.node).find(|&a| d.is(a, ns::W, "p")) {
                        push(Loc::new(l.part, p));
                    }
                    continue;
                }
                _ => continue,
            };
            let d = doc.part(within.part);
            for p in d.descendants_named(within.node, ns::W, "p") {
                push(Loc::new(within.part, p));
            }
        }
        let locs: Vec<Loc> = paras.iter().map(|(_, l)| *l).collect();
        let blocks = proj.para_blocks(doc, anchors, &opts, &locs);
        let only: HashSet<&str> = paras.iter().map(|(h, _)| h.as_str()).collect();
        let in_cell: HashSet<&str> = paras
            .iter()
            .filter(|(_, l)| {
                let d = doc.part(l.part);
                d.ancestors(l.node).any(|a| d.is(a, ns::W, "tc"))
            })
            .map(|(h, _)| h.as_str())
            .collect();
        let gone_here: HashSet<&str> = blocks
            .iter()
            .filter_map(|b| if let Block::P(p) = b { gone(p).then_some(p.id.as_str()) } else { None })
            .collect();
        let after = proj.bookmarks();
        let wide = bookmarks_before.iter().any(|(name, was)| after.get(name) != Some(was))
            || after.values().any(|t| gone_here.contains(t.as_str()));
        let handles: Vec<&str> = paras.iter().map(|(h, _)| h.as_str()).collect();
        let fields =
            if wide { proj.fields(doc, anchors, &opts) } else { proj.fields_for(doc, anchors, &opts, &handles) };
        let Ok(fields) = fields else { return Vec::new() };
        // Whether a reference target is a paragraph deleted with tracked changes.
        let deleted = |t: &str| {
            gone_here.contains(t)
                || match anchors.resolve(doc, t) {
                    Some(Target::Block { para, .. }) => {
                        matches!(proj.para_blocks(doc, anchors, &opts, &[para]).first(), Some(Block::P(p)) if gone(p))
                    }
                    _ => false,
                }
        };
        let mut out = Vec::new();
        for r in &fields.refs {
            if only.contains(r.anchor.as_str()) {
                out.extend(ref_finding(r, &deleted));
                continue;
            }
            let Some(was) = bookmarks_before.get(&r.bookmark) else { continue };
            match r.target.as_deref() {
                None => out.push(broken_ref(r)),
                Some(now) if gone_here.contains(now) => out.extend(ref_finding(r, &|_| true)),
                Some(now) if now != was && removed.iter().any(|h| h == was) => out.push(Finding::new(
                    "broken_ref",
                    "warning",
                    &r.anchor,
                    format!("{} {} pointed to {was}, which was deleted; it now points to {now}", r.typ, r.bookmark),
                )),
                _ => {}
            }
        }
        out.extend(fields.placeholders.iter().filter(|p| only.contains(p.anchor.as_str())).map(placeholder_finding));
        for b in &blocks {
            match b {
                Block::P(p) => {
                    if let Some(h) = p.heading {
                        let in_body = body.is_some_and(|bi| matches!(anchors.resolve(doc, &p.id), Some(Target::Block { story, .. }) if story == bi));
                        let walk = if in_body { proj.body_headings(doc, anchors, &opts, &p.id) } else { Vec::new() };
                        // The paragraph itself is last unless it sits in a table.
                        if walk.last().is_some_and(|(x, _)| *x == p.id) {
                            let prev = walk[..walk.len() - 1].iter().rev().find_map(|(_, l)| *l);
                            out.extend(heading_finding(&p.id, h, prev));
                        }
                    }
                    self.para_findings(p, in_cell.contains(p.id.as_str()), &mut out);
                    for img in p.inlines.iter().filter_map(|i| i.image.as_deref()) {
                        out.extend(alt_finding(img));
                    }
                }
                Block::Image(im) => out.extend(alt_finding(&im.image)),
                Block::Table(_) => {}
            }
        }
        let mut seen = Vec::new();
        for f in &out {
            if !seen.contains(&f.anchor) {
                seen.push(f.anchor.clone());
            }
        }
        let pos: HashMap<String, usize> =
            self.in_document_order(seen).into_iter().enumerate().map(|(i, a)| (a, i)).collect();
        sort_findings(&mut out, &pos);
        out.dedup();
        out
    }
}

/// A paragraph deleted with tracked changes, all of its text with it.
fn gone(p: &Paragraph) -> bool {
    p.mark.as_ref().is_some_and(|m| m.typ == "del" || m.typ == "moveFrom") && para_text(&p.inlines).is_empty()
}

/// `broken_ref` for a reference whose bookmark is gone or whose target `deleted` says is deleted
/// with tracked changes.
fn ref_finding(r: &project::RefField, deleted: &dyn Fn(&str) -> bool) -> Option<Finding> {
    match r.target.as_deref() {
        None => Some(broken_ref(r)),
        Some(t) if deleted(t) => Some(Finding::new(
            "broken_ref",
            "warning",
            &r.anchor,
            format!("{} {} points to {t}, which is deleted with tracked changes", r.typ, r.bookmark),
        )),
        _ => None,
    }
}

fn placeholder_finding(p: &project::Placeholder) -> Finding {
    let blank = p.text.starts_with(['.', '…', '_']);
    let (sev, what) = if blank { ("info", "blank to fill in") } else { ("warning", "placeholder left in the text") };
    Finding::new("leftover_placeholder", sev, &p.anchor, format!("{what}: \"{}\"", cut(&p.text, 40)))
}

fn heading_finding(id: &str, h: u8, prev: Option<u8>) -> Option<Finding> {
    let prev = prev?;
    (h > prev + 1).then(|| {
        Finding::new("heading_order", "warning", id, format!("H{h} follows H{prev}; H{} is skipped", prev + 1))
    })
}

fn alt_finding(img: &ImageBase) -> Option<Finding> {
    let picture = img.textbox.is_none() && (img.asset.is_some() || img.external);
    (picture && !img.decorative && img.alt.as_deref().is_none_or(|a| a.trim().is_empty())).then(|| {
        Finding::new(
            "missing_alt",
            "warning",
            &img.id,
            "image without alt text (set_alt_text, or mark it decorative)".into(),
        )
    })
}

fn broken_ref(r: &project::RefField) -> Finding {
    Finding::new(
        "broken_ref",
        "warning",
        &r.anchor,
        format!("{} {} points to a bookmark that is gone (shows \"{}\")", r.typ, r.bookmark, cut(&r.result, 40)),
    )
}

fn double_space_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // Exactly two spaces between words: longer runs line things up on purpose.
    R.get_or_init(|| Regex::new(r"[^\s]  [^\s]").expect("double space regex"))
}

fn sort_findings(found: &mut [Finding], pos: &HashMap<String, usize>) {
    found.sort_by_key(|f| pos.get(&f.anchor).copied().unwrap_or(usize::MAX));
}

/// `lint`'s text: one `§ lint:` line per finding, or `§ lint: none`.
fn lint_text(found: &[Finding]) -> String {
    if found.is_empty() {
        return "§ lint: none".into();
    }
    let mut lines: Vec<String> = found.iter().take(LINT_MAX).map(Finding::line).collect();
    if found.len() > LINT_MAX {
        lines.push(format!("§ lint: … (+{} more)", found.len() - LINT_MAX));
    }
    lines.join("\n")
}

/// Every image of every story (not comments): story, paragraph, the image and a block image's
/// revision.
fn each_image<'a>(vm: &'a ViewModel, f: &mut dyn FnMut(&'a str, &'a str, &'a ImageBase, Option<&'a Revision>)) {
    each_block(vm, &mut |story, _, b| match b {
        Block::P(p) => {
            for i in &p.inlines {
                if let Some(img) = i.image.as_deref() {
                    f(story, &p.id, img, i.rev.as_ref());
                }
            }
        }
        Block::Image(im) => f(story, &im.para, &im.image, im.rev.as_ref()),
        Block::Table(_) => {}
    });
}

/// Revisions in document order, one entry per revision id and type.
#[derive(Default)]
struct Revs {
    list: Vec<RevEntry>,
    at: HashMap<(u64, String), usize>,
}

struct RevEntry {
    rev: Revision,
    story: String,
    anchors: Vec<String>,
    text: String,
}

impl Revs {
    fn add(&mut self, r: &Revision, story: &str, anchor: &str, text: &str) {
        let key = (r.id, r.typ.clone());
        let i = *self.at.entry(key).or_insert_with(|| {
            self.list.push(RevEntry {
                rev: r.clone(),
                story: story.to_owned(),
                anchors: Vec::new(),
                text: String::new(),
            });
            self.list.len() - 1
        });
        let e = &mut self.list[i];
        if !e.anchors.iter().any(|a| a == anchor) {
            e.anchors.push(anchor.to_owned());
        }
        if e.text.chars().count() <= REV_TEXT {
            e.text.push_str(text);
        }
    }

    fn block(&mut self, story: &str, b: &Block) {
        match b {
            Block::P(p) => {
                for i in &p.inlines {
                    let t = match i.k.as_str() {
                        "text" | "field" => i.t.as_deref().unwrap_or(""),
                        "tab" => "\t",
                        "br" => "\n",
                        _ => "",
                    };
                    if let Some(r) = &i.rev {
                        self.add(r, story, &p.id, t);
                    }
                    if let Some(r) = &i.fmt_change {
                        self.add(r, story, &p.id, t);
                    }
                }
                if let Some(r) = &p.mark {
                    self.add(r, story, &p.id, "");
                }
                if let Some(r) = &p.ppr_change {
                    self.add(r, story, &p.id, "");
                }
            }
            Block::Table(t) => {
                for row in &t.rows {
                    if let Some(r) = &row.rev {
                        self.add(r, story, &row.id, "");
                    }
                }
            }
            Block::Image(im) => {
                if let Some(r) = &im.rev {
                    self.add(r, story, &im.image.id, "");
                }
            }
        }
    }
}

// ------------------------------------------------------------------------------ help

/// `help`: the read ops' and edit ops' one-line signatures (the `usage` INVALID_OP carries),
/// or one op's.
fn help(obj: &Map<String, Value>) -> Result<Answer, OpError> {
    let line = |name: &str| -> Option<String> {
        if let Some((_, u)) = READ_USAGE.iter().find(|(n, _)| *n == name) {
            return Some((*u).to_owned());
        }
        ops::op_spec(name).map(|s| {
            let mut l = ops::usage(s.name);
            if !handlers::IMPLEMENTED.contains(&s.name) {
                l.push_str(" (not implemented)");
            }
            l
        })
    };
    match obj.get("name") {
        None | Some(Value::Null) => {
            let mut out = vec!["§ read ops".to_owned()];
            out.extend(READ_USAGE.iter().map(|(_, u)| (*u).to_owned()));
            out.push("§ edit ops".to_owned());
            out.extend(ops::OPS.iter().filter_map(|s| line(s.name)));
            Ok(Answer::Text(out.join("\n")))
        }
        Some(Value::String(n)) if !n.is_empty() => match line(n) {
            Some(l) => Ok(Answer::Text(l)),
            None => Err(bad("help", "/name", format!("unknown op {n:?}; help with no name lists them all"))),
        },
        Some(_) => Err(bad("help", "/name", "name is an op name (replace_text, list_changes, …)")),
    }
}

// ------------------------------------------------------------------------------ base64

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub(crate) fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn id_checks() {
        assert!(is_image_anchor("img3") && !is_image_anchor("img0") && !is_image_anchor("img"));
        assert!(is_asset_id("a_0123456789abcdef") && !is_asset_id("a_0123456789ABCDEF"));
        assert!(is_change_set_id("cs_7") && !is_change_set_id("cs_") && !is_change_set_id("cs_A"));
        assert!(anchor_re().is_match("t1.r2.c3") && anchor_re().is_match("h1-1x") && !anchor_re().is_match("x1"));
    }
}
