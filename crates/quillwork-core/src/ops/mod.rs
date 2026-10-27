//! Edit ops: the mutating op catalogue (spec/schema/ops.schema.json), dispatched one at a time by
//! the engine ([`crate::engine`]) inside an all-or-nothing transaction.
//!
//! - [`find`]: locating text in blocks (exact, then whitespace / quote folded).
//! - [`text`]: the replace core (word-level diff, formatting inheritance, tracked runs),
//!   `replace_text`, `find_and_replace`, `split_paragraph`.
//! - [`para`]: block ops (`insert_paragraph`, `delete_block`, `move_block`, `merge_paragraphs`,
//!   `set_paragraph`, `insert_break`).
//! - [`format`]: the `set_text_format` op.
//! - [`list`]: `set_list`, `restart_numbering`, `change_level`.
//! - [`handlers`]: the op table, including the bridges to [`crate::edit`] (review, comments,
//!   tables).
//! - [`render`]: the minimal projection lines op results and errors carry.

// `OpError` carries every per-code field of spec/errors.md (~270 bytes). It is built at most once
// per transaction, on the failure path, so boxing it would buy nothing but noise.
#![allow(clippy::result_large_err)]

pub mod find;
pub mod format;
pub mod handlers;
pub mod list;
pub mod para;
pub mod render;
pub mod text;

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::Value;

use crate::anchors::{Anchors, Target, is_block_anchor};
use crate::edit::Mode;
use crate::error::Error;
use crate::track::{IdAlloc, Stamp};
use crate::wml::text::TextMode;
use crate::wml::{Loc, PartIdx, WordDoc};
use crate::xml::{Document, NodeId, ns};

// ---------------------------------------------------------------------- errors

/// One `AMBIGUOUS` candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub anchor: String,
    pub occurrence: Option<u32>,
    pub context: String,
}

/// `LIMIT` details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimitInfo {
    pub name: String,
    pub max: i64,
    pub actual: i64,
}

/// A failed op, with the per-code fields of spec/errors.md. The engine adds `version`,
/// `op_index`, `op`, `retryable` and renders `current` (after rollback) for errors that carry
/// one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpError {
    pub code: &'static str,
    pub message: String,
    pub anchor: Option<String>,
    /// Render fresh projection lines of `anchor` into `current` (STALE_ANCHOR, IN_FIELD, …).
    pub wants_current: bool,
    pub current: Option<Vec<String>>,
    pub candidates: Option<Vec<Candidate>>,
    pub what: Option<&'static str>,
    pub reason: Option<String>,
    pub path: Option<String>,
    pub usage: Option<String>,
    pub limit: Option<LimitInfo>,
    pub invariant: Option<String>,
}

impl OpError {
    pub fn new(code: &'static str, message: impl Into<String>) -> OpError {
        OpError { code, message: message.into(), ..OpError::default() }
    }

    pub fn invalid(path: impl Into<String>, message: impl Into<String>) -> OpError {
        OpError { path: Some(path.into()), ..OpError::new("INVALID_OP", message) }
    }

    pub fn not_found(what: &'static str, message: impl Into<String>) -> OpError {
        OpError { what: Some(what), ..OpError::new("NOT_FOUND", message) }
    }

    pub fn unsupported(reason: &str, message: impl Into<String>) -> OpError {
        OpError { reason: Some(reason.to_owned()), ..OpError::new("UNSUPPORTED", message) }
    }

    pub fn in_field(anchor: &str, reason: &str, message: impl Into<String>) -> OpError {
        OpError {
            anchor: Some(anchor.to_owned()),
            wants_current: true,
            reason: Some(reason.to_owned()),
            ..OpError::new("IN_FIELD", message)
        }
    }

    pub fn with_anchor(mut self, anchor: &str) -> OpError {
        if self.anchor.is_none() {
            self.anchor = Some(anchor.to_owned());
        }
        self
    }

    /// Whether the same intent can succeed with other arguments (spec/errors.md).
    pub fn retryable(&self) -> bool {
        match self.code {
            "PROTECTED" | "BAD_PACKAGE" | "INVARIANT" => false,
            "UNSUPPORTED" => matches!(
                self.reason.as_deref(),
                Some("direct_only" | "track_only" | "unsupported_target" | "uneven_split")
            ),
            _ => true,
        }
    }
}

/// Reasons an `edit::*` message may carry as a `reason: ` prefix.
const REASONS: &[&str] = &[
    "signed",
    "encrypted",
    "irm",
    "strict",
    "document_protection",
    "locked_control",
    "track_only",
    "direct_only",
    "preserve_only",
    "field_result",
    "beta",
    "not_implemented",
    "merged_cells",
    "nested_table",
    "no_grid",
    "unaligned_cells",
    "uneven_split",
    "unsupported_target",
];

fn split_reason(message: &str) -> (Option<&'static str>, &str) {
    if let Some((head, rest)) = message.split_once(':')
        && let Some(r) = REASONS.iter().find(|r| **r == head.trim())
    {
        return (Some(r), rest.trim_start());
    }
    (None, message)
}

fn infer_what(message: &str) -> &'static str {
    let m = message.to_ascii_lowercase();
    if m.contains("change set") {
        "change_set"
    } else if m.contains("revision") {
        "revision"
    } else if m.contains("comment") {
        "comment"
    } else if m.contains("style") {
        "style"
    } else if m.contains("part") {
        "part"
    } else if m.contains(" not found in ") || m.contains("occurrence") {
        "text"
    } else {
        "anchor"
    }
}

impl From<Error> for OpError {
    fn from(e: Error) -> OpError {
        match e {
            Error::Edit { code, message } => {
                let (reason, _) = split_reason(&message);
                let mut o = OpError::new(code, message.clone());
                match code {
                    "UNSUPPORTED" => o.reason = Some(reason.unwrap_or("unsupported_target").to_owned()),
                    "CONFIRM_REQUIRED" => o.reason = Some(reason.unwrap_or("beta").to_owned()),
                    "PROTECTED" | "IN_FIELD" => o.reason = reason.map(str::to_owned),
                    "NOT_FOUND" => o.what = Some(infer_what(&message)),
                    _ => {}
                }
                o
            }
            Error::LimitExceeded { name, max, actual } => OpError {
                limit: Some(LimitInfo { name: name.clone(), max, actual }),
                ..OpError::new("LIMIT", format!("{name}: {actual} exceeds the limit of {max}"))
            },
            Error::NotFound(m) => OpError::not_found("anchor", m),
            other => OpError::new("INTERNAL", other.to_string()),
        }
    }
}

pub type OpResult<T> = std::result::Result<T, OpError>;

// ---------------------------------------------------------------------- tracking context

/// Revision bookkeeping for one transaction.
pub struct Tk<'a> {
    pub ids: &'a mut IdAlloc,
    pub stamp: Stamp,
    /// Mode of the op being applied.
    pub mode: Mode,
    /// Ids of the `w:ins` / `w:del` / marks this transaction wrote (editing inside one of our own
    /// insertions removes text instead of tracking a deletion).
    pub own: HashSet<u32>,
    /// Revision ids that belong to the transaction's change set.
    pub revisions: Vec<u32>,
    /// `(original, copy)`: a revision element of an earlier change set was split and the copy got
    /// a fresh id; the copy joins the original's change set at commit.
    pub aliases: Vec<(u32, u32)>,
    /// Ids of earlier pending insertions by this author that were edited in place (their change
    /// sets change too).
    pub absorbed: Vec<u32>,
    pub warnings: Vec<String>,
}

impl<'a> Tk<'a> {
    pub fn new(ids: &'a mut IdAlloc, stamp: Stamp) -> Tk<'a> {
        Tk {
            ids,
            stamp,
            mode: Mode::Track,
            own: HashSet::new(),
            revisions: Vec::new(),
            aliases: Vec::new(),
            absorbed: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn track(&self) -> bool {
        self.mode == Mode::Track
    }

    /// A revision id of this change set.
    pub fn rev(&mut self) -> u32 {
        let id = self.ids.alloc();
        self.own.insert(id);
        self.revisions.push(id);
        id
    }

    /// A fresh id for a copy of someone else's revision element (split wrappers).
    pub fn foreign(&mut self) -> u32 {
        self.ids.alloc()
    }

    pub fn is_own(&self, d: &Document, n: NodeId) -> bool {
        rev_id(d, n).is_some_and(|id| self.own.contains(&id))
    }

    /// A pending `w:ins` this author may rewrite in place: one of this transaction's, or an
    /// earlier one with the same `w:author` (Word collapses same-author edits the same way).
    pub fn is_mine(&self, d: &Document, n: NodeId) -> bool {
        crate::track::write::is_w(d, n, "ins")
            && (self.is_own(d, n)
                || crate::track::write::w_attr(d, n, "author").as_deref() == Some(self.stamp.author.as_str()))
    }

    /// Note that the pending insertion `n` (see [`Tk::is_mine`]) is being edited in place.
    pub fn absorb(&mut self, d: &Document, n: NodeId) {
        if let Some(id) = rev_id(d, n)
            && !self.own.contains(&id)
            && !self.absorbed.contains(&id)
        {
            self.absorbed.push(id);
        }
    }
}

/// The `w:id` of a revision element.
pub fn rev_id(d: &Document, n: NodeId) -> Option<u32> {
    crate::track::write::w_attr(d, n, "id").and_then(|v| v.trim().parse::<u32>().ok())
}

/// What a handler gets.
pub struct Cx<'a, 'b> {
    pub doc: &'a mut WordDoc,
    pub anchors: &'a mut Anchors,
    pub tk: &'a mut Tk<'b>,
    pub index: usize,
    /// Change set id → its revision ids (accept / reject by change set).
    pub change_sets: &'a HashMap<String, Vec<u32>>,
    /// Transaction-level author (comment ops use the op's, then this).
    pub author: String,
}

impl Cx<'_, '_> {
    pub fn path(&self, key: &str) -> String {
        format!("/ops/{}/{key}", self.index)
    }
}

// ---------------------------------------------------------------------- op table

/// One catalogue op: its argument keys (besides `op` and the guards).
pub struct OpSpec {
    pub name: &'static str,
    pub required: &'static [&'static str],
    pub optional: &'static [&'static str],
    /// Exactly one of these (when non-empty).
    pub one_of: &'static [&'static str],
}

const GUARD_KEYS: &[&str] = &["op", "expect", "base_version", "mode"];

macro_rules! spec {
    ($name:literal, [$($r:literal),*], [$($o:literal),*]) => {
        OpSpec { name: $name, required: &[$($r),*], optional: &[$($o),*], one_of: &[] }
    };
    ($name:literal, [$($r:literal),*], [$($o:literal),*], one_of [$($x:literal),*]) => {
        OpSpec { name: $name, required: &[$($r),*], optional: &[$($o),*], one_of: &[$($x),*] }
    };
}

/// Every mutating op of the catalogue (implemented or not).
pub const OPS: &[OpSpec] = &[
    spec!("replace_text", ["anchor", "replace"], ["find", "occurrence"]),
    spec!("find_and_replace", ["find", "replace", "count"], ["anchor"]),
    spec!("insert_paragraph", ["text"], ["after", "before", "style", "list"], one_of ["after", "before"]),
    spec!("delete_block", ["anchor"], []),
    spec!("move_block", ["anchor"], ["after", "before"], one_of ["after", "before"]),
    spec!("split_paragraph", ["anchor", "at_text"], ["occurrence"]),
    spec!("merge_paragraphs", ["first", "second"], []),
    spec!("set_text_format", ["anchor", "marks"], ["find", "occurrence"]),
    spec!("set_paragraph", ["anchor"], ["style", "align", "indent", "spacing", "keep_with_next", "page_break_before"]),
    spec!("insert_break", ["after", "kind"], ["section_type"]),
    spec!("insert_footnote", ["anchor", "find", "text"], ["occurrence", "endnote"]),
    spec!("set_hyperlink", ["anchor", "find"], ["occurrence", "url", "bookmark", "remove"]),
    spec!("insert_field", ["anchor", "kind"], ["find", "occurrence", "args"]),
    spec!("set_list", ["anchor", "list"], ["level"]),
    spec!("restart_numbering", ["anchor", "start"], []),
    spec!("change_level", ["anchor"], ["delta", "level"], one_of ["delta", "level"]),
    spec!("insert_table", ["after", "rows", "cols"], ["header_row", "style", "data"]),
    spec!("insert_row", [], ["after", "before", "cells"], one_of ["after", "before"]),
    spec!("delete_row", ["row"], []),
    spec!("set_cell", ["cell", "text"], []),
    spec!("insert_column", ["table", "at"], ["cells", "confirm"]),
    spec!("delete_column", ["table", "column"], ["confirm"]),
    spec!("merge_cells", ["from", "to"], []),
    spec!("split_cell", ["cell"], ["rows", "cols"]),
    spec!("insert_image", ["asset"], ["after", "anchor", "find", "occurrence", "width", "alt", "decorative", "wrap"]),
    spec!("replace_image", ["image", "asset"], ["fit"]),
    spec!("resize_image", ["image"], ["width", "height", "scale"]),
    spec!("set_alt_text", ["image"], ["alt", "title", "decorative"]),
    spec!("delete_image", ["image"], []),
    spec!("anchor_image", ["image", "wrap"], ["align"]),
    spec!("add_comment", ["anchor", "text"], ["find", "occurrence", "author"]),
    spec!("reply_comment", ["comment", "text"], ["author"]),
    spec!("edit_comment", ["comment", "text"], []),
    spec!("resolve_comment", ["comment", "done"], []),
    spec!("delete_comment", ["comment"], []),
    spec!("accept", [], ["change_set", "revision_ids", "author", "all"], one_of ["change_set", "revision_ids", "author", "all"]),
    spec!("reject", [], ["change_set", "revision_ids", "author", "all"], one_of ["change_set", "revision_ids", "author", "all"]),
    spec!("compare", ["other"], ["author"]),
    spec!("fill", ["values"], []),
    spec!("set_properties", ["properties"], []),
    spec!("set_track_changes", ["on"], []),
];

pub fn op_spec(name: &str) -> Option<&'static OpSpec> {
    OPS.iter().find(|s| s.name == name)
}

/// `name {req, opt?}` — the one-line signature INVALID_OP carries as `usage`.
pub fn usage(name: &str) -> String {
    let Some(s) = op_spec(name) else { return format!("{name} {{…}}") };
    let mut parts: Vec<String> = Vec::new();
    if !s.one_of.is_empty() && s.one_of.iter().all(|k| s.optional.contains(k)) {
        parts.push(s.one_of.join("|"));
    }
    for r in s.required {
        parts.push((*r).to_owned());
    }
    for o in s.optional {
        if !s.one_of.contains(o) || !s.one_of.iter().all(|k| s.optional.contains(k)) {
            parts.push(format!("{o}?"));
        }
    }
    parts.push("expect?".to_owned());
    format!("{name} {{{}}}", parts.join(", "))
}

/// Check one op's shape (keys present / allowed / exclusive). Returns its name.
pub fn check_shape(index: usize, op: &Value) -> OpResult<&'static OpSpec> {
    let obj = op.as_object().ok_or_else(|| OpError {
        usage: Some("{op, …}".to_owned()),
        ..OpError::invalid(format!("/ops/{index}"), "an op must be a JSON object")
    })?;
    let name = obj.get("op").and_then(Value::as_str).unwrap_or("");
    let spec = op_spec(name).ok_or_else(|| OpError {
        usage: Some("op is one of the catalogue ops (advanced_op help lists them)".to_owned()),
        ..OpError::invalid(format!("/ops/{index}/op"), format!("unknown op {name:?}"))
    })?;
    let bad = |key: &str, msg: String| OpError {
        usage: Some(usage(spec.name)),
        ..OpError::invalid(format!("/ops/{index}/{key}"), msg)
    };
    for key in obj.keys() {
        let k = key.as_str();
        if !GUARD_KEYS.contains(&k) && !spec.required.contains(&k) && !spec.optional.contains(&k) {
            return Err(bad(k, format!("{} takes no argument {k:?}", spec.name)));
        }
    }
    for r in spec.required {
        if obj.get(*r).is_none_or(Value::is_null) {
            return Err(bad(r, format!("{} needs {r}", spec.name)));
        }
    }
    if !spec.one_of.is_empty() {
        let given: Vec<&str> =
            spec.one_of.iter().copied().filter(|k| obj.get(*k).is_some_and(|v| !v.is_null())).collect();
        // accept / reject: `all: false` is not a selector.
        let given: Vec<&str> =
            given.into_iter().filter(|k| !(*k == "all" && obj.get("all") == Some(&Value::Bool(false)))).collect();
        let needs_one = !matches!(spec.name, "insert_row");
        if given.len() > 1 {
            return Err(bad(given[1], format!("give only one of {}", spec.one_of.join(", "))));
        }
        if given.is_empty() && needs_one {
            return Err(bad(spec.one_of[0], format!("{} needs one of {}", spec.name, spec.one_of.join(", "))));
        }
        if given.is_empty() && spec.name == "insert_row" {
            return Err(bad("after", "insert_row needs after or before".to_owned()));
        }
    }
    if let Some(m) = obj.get("mode")
        && !matches!(m.as_str(), Some("track" | "direct"))
    {
        return Err(bad("mode", "mode is track or direct".to_owned()));
    }
    if let Some(e) = obj.get("expect") {
        let ok = match e.as_object() {
            Some(o) if o.len() == 1 => match (o.get("text"), o.get("hash")) {
                (Some(Value::String(_)), None) => true,
                (None, Some(Value::String(h))) => valid_hash(h),
                _ => false,
            },
            _ => false,
        };
        if !ok {
            return Err(bad("expect", "expect is {text} or {hash: \"sha256:<12-64 hex>\"}".to_owned()));
        }
    }
    Ok(spec)
}

fn valid_hash(h: &str) -> bool {
    match h.strip_prefix("sha256:") {
        Some(hex) => {
            (12..=64).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        None => false,
    }
}

/// Deserialize an op's arguments into `T`, turning type errors into INVALID_OP.
pub fn args<T: serde::de::DeserializeOwned>(index: usize, op: &Value) -> OpResult<T> {
    serde_json::from_value(op.clone()).map_err(|e| {
        let msg = e.to_string();
        // serde names the field in backticks: "invalid type: … for field `x`" is rare; find any key.
        let key = op
            .as_object()
            .and_then(|o| o.keys().find(|k| msg.contains(&format!("`{k}`"))).cloned())
            .unwrap_or_default();
        let name = op.get("op").and_then(Value::as_str).unwrap_or("");
        OpError {
            usage: Some(usage(name)),
            ..OpError::invalid(
                if key.is_empty() { format!("/ops/{index}") } else { format!("/ops/{index}/{key}") },
                msg,
            )
        }
    })
}

// ---------------------------------------------------------------------- targets

/// A resolved anchor (or `a..b` range): the paragraphs it covers.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub handle: String,
    pub part: PartIdx,
    pub target: Target,
    /// Paragraphs covered (one for a block; the cell / row / table paragraphs; a range's).
    pub paras: Vec<NodeId>,
    /// The table for a table / row / cell anchor.
    pub table: Option<NodeId>,
    /// Handles whose change since `base_version` makes this target stale.
    pub watch: Vec<String>,
}

impl Resolved {
    pub fn single_para(&self) -> Option<NodeId> {
        match self.target {
            Target::Block { para, .. } => Some(para.node),
            _ if self.paras.len() == 1 => Some(self.paras[0]),
            _ => None,
        }
    }
}

/// Resolve `handle` (an anchor or a `a..b` range of block anchors). `key` names the argument for
/// error paths.
pub fn resolve(doc: &WordDoc, anchors: &Anchors, index: usize, key: &str, handle: &str) -> OpResult<Resolved> {
    if let Some((a, b)) = handle.split_once("..") {
        return resolve_range(doc, anchors, index, key, handle, a, b);
    }
    let target = anchors.resolve(doc, handle).ok_or_else(|| {
        OpError::not_found("anchor", format!("no anchor {handle} at this version")).with_anchor(handle)
    })?;
    let (part, paras, table, watch) = match target {
        Target::Block { para, .. } => (para.part, vec![para.node], None, vec![handle.to_owned()]),
        Target::Table { table } => {
            (table.part, paras_in(doc.part(table.part), table.node), Some(table.node), vec![handle.to_owned()])
        }
        Target::Row { table, row } | Target::Cell { table, cell: row, .. } => {
            let th = anchors.handle_of(table).unwrap_or(handle).to_owned();
            (table.part, paras_in(doc.part(table.part), row.node), Some(table.node), vec![th])
        }
        Target::Image { drawing } => {
            let d = doc.part(drawing.part);
            let p = d.ancestors(drawing.node).find(|&a| d.is(a, ns::W, "p"));
            match p {
                Some(p) => {
                    let h = anchors.handle_of(Loc::new(drawing.part, p)).unwrap_or(handle).to_owned();
                    (drawing.part, vec![p], None, vec![h])
                }
                None => return Err(OpError::unsupported("unsupported_target", "image outside a paragraph")),
            }
        }
        Target::Comment { comment, .. } => {
            (comment.part, paras_in(doc.part(comment.part), comment.node), None, vec![handle.to_owned()])
        }
    };
    Ok(Resolved { handle: handle.to_owned(), part, target, paras, table, watch })
}

fn resolve_range(
    doc: &WordDoc,
    anchors: &Anchors,
    index: usize,
    key: &str,
    handle: &str,
    a: &str,
    b: &str,
) -> OpResult<Resolved> {
    let path = format!("/ops/{index}/{key}");
    if !is_block_anchor(a) || !is_block_anchor(b) {
        return Err(OpError::invalid(path, "a range joins two block anchors (b12..b19)"));
    }
    let ta =
        anchors.resolve(doc, a).ok_or_else(|| OpError::not_found("anchor", format!("no anchor {a}")).with_anchor(a))?;
    let tb =
        anchors.resolve(doc, b).ok_or_else(|| OpError::not_found("anchor", format!("no anchor {b}")).with_anchor(b))?;
    let (Target::Block { para: pa, .. }, Target::Block { para: pb, .. }) = (ta, tb) else {
        return Err(OpError::invalid(path, "a range joins two block anchors"));
    };
    let d = doc.part(pa.part);
    if pa.part != pb.part || d.parent(pa.node) != d.parent(pb.node) {
        return Err(OpError::invalid(path, "a range must stay within one story and container"));
    }
    let mut paras = Vec::new();
    let mut cur = Some(pa.node);
    let mut found = false;
    while let Some(n) = cur {
        if d.is(n, ns::W, "p") {
            paras.push(n);
        }
        if n == pb.node {
            found = true;
            break;
        }
        cur = d.next_sibling(n);
    }
    if !found {
        return Err(OpError::invalid(path, format!("{a} comes after {b}")));
    }
    let watch = paras.iter().filter_map(|&p| anchors.handle_of(Loc::new(pa.part, p)).map(str::to_owned)).collect();
    Ok(Resolved { handle: handle.to_owned(), part: pa.part, target: ta, paras, table: None, watch })
}

/// Every `w:p` below `n` (cell paragraphs included), in document order.
pub fn paras_in(d: &Document, n: NodeId) -> Vec<NodeId> {
    if d.is(n, ns::W, "p") {
        return vec![n];
    }
    let mut out = Vec::new();
    crate::edit::walk(d, n, |c| {
        if crate::edit::is_ignored_branch(d, c) {
            return false;
        }
        if d.is(c, ns::W, "p") {
            out.push(c);
            return false;
        }
        !matches!(crate::track::write::w_local(d, c), Some("txbxContent" | "pPr" | "rPr" | "sectPr"))
    });
    out
}

/// Accepted-mode text of a resolved target (paragraphs joined with `\n`): what `expect` guards.
pub fn target_text(doc: &WordDoc, r: &Resolved) -> String {
    let d = doc.part(r.part);
    r.paras.iter().map(|&p| crate::wml::text::text(d, p, TextMode::AcceptAll)).collect::<Vec<_>>().join("\n")
}

/// The anchor-ish argument an op targets (for `expect` and staleness), with its key.
pub fn target_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "replace_text" | "delete_block" | "move_block" | "split_paragraph" | "set_text_format" | "set_paragraph"
        | "set_list" | "restart_numbering" | "change_level" | "insert_footnote" | "set_hyperlink" | "insert_field"
        | "add_comment" => "anchor",
        "find_and_replace" => "anchor",
        "merge_paragraphs" => "first",
        "insert_paragraph" | "insert_break" | "insert_table" => "after",
        "insert_row" => "after",
        "delete_row" => "row",
        "set_cell" | "split_cell" => "cell",
        "insert_column" | "delete_column" => "table",
        "merge_cells" => "from",
        "replace_image" | "resize_image" | "set_alt_text" | "delete_image" | "anchor_image" => "image",
        _ => return None,
    })
}
