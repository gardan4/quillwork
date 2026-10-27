//! The engine facade: one open document, its anchors and change-set log, and all-or-nothing
//! transactions of catalogue ops (spec/schema/ops.schema.json `Transaction`).
//!
//! ```text
//! let mut e = Engine::open(bytes, None)?;
//! let result_json = e.apply(r#"{"base_version": 0, "ops": [...]}"#);   // OpResult or error
//! let (docx, sidecar) = e.save()?;                                     // persist both
//! let e = Engine::open(docx, Some(&sidecar))?;                         // same anchors, log, version
//! ```
//!
//! A transaction is checked (shape, `base_version`, per-op `expect` / staleness), applied op by
//! op, validated ([`crate::validate`]: no new error-severity finding), and then committed as one
//! version and at most one change set. Any failure rolls the document, anchors and id allocator
//! back to where they were; the error envelope (spec/schema/error.schema.json) says why.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::anchors::{Anchors, Sidecar, Target};
use crate::edit::{EditOutcome, Mode};
use crate::error::{Error, Result};
use crate::ops::render::{RenderOpts, render_error_text, render_lines, render_result_text};
use crate::ops::{self, Cx, OpError, Tk, handlers};
use crate::project::vm::ViewModel;
use crate::project::{
    self, ChangeSetInfo, FindRequest, ProjectOptions, Projector, ReadMode, ReadRequest, ReadResult, View,
};
use crate::track::{IdAlloc, Stamp, now_secs};
use crate::validate;
use crate::wml::{PartIdx, WordDoc};
use crate::xml::{self, ns};

mod reads;

/// The spec version results and errors carry.
pub const SPEC_VERSION: &str = "0.1.0";
/// `LIMIT ops_per_transaction`.
pub const MAX_OPS: usize = 200;
/// Author stamped on revisions when the transaction names none.
pub const DEFAULT_AGENT: &str = "AI";

const SIDECAR_FORMAT: u32 = 1;
const TX_KEYS: &[&str] = &["spec_version", "base_version", "ops", "mode", "label", "author", "turn", "projection_mode"];

/// The one-line signature of a transaction (the `usage` of request-level INVALID_OP errors).
pub const TX_USAGE: &str = "{base_version, ops, mode?, label?, author?, turn?, projection_mode?, spec_version?}";

/// One committed change set (one transaction's tracked revisions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSetRec {
    pub id: String,
    pub label: Option<String>,
    pub author: String,
    pub turn: Option<String>,
    pub version: u64,
    pub revisions: Vec<u32>,
    #[serde(default)]
    pub accepted: Vec<u32>,
    #[serde(default)]
    pub rejected: Vec<u32>,
}

impl ChangeSetRec {
    /// open | accepted | rejected | partial.
    pub fn status(&self) -> &'static str {
        let done = |id: &u32| self.accepted.contains(id) || self.rejected.contains(id);
        if !self.revisions.iter().any(done) {
            "open"
        } else if !self.revisions.iter().all(done) {
            "partial"
        } else if self.revisions.iter().all(|id| self.accepted.contains(id)) {
            "accepted"
        } else if self.revisions.iter().all(|id| self.rejected.contains(id)) {
            "rejected"
        } else {
            "partial"
        }
    }

    /// The view-model `ChangeSet` object.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "author": self.author,
            "turn": self.turn,
            "version": self.version,
            "revisions": self.revisions,
            "untracked": [],
            "status": self.status(),
        })
    }
}

/// Engine state persisted next to the anchors.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct EngineState {
    version: u64,
    next_cs: u64,
    change_sets: Vec<ChangeSetRec>,
    /// Handle → the version it last changed at.
    last_changed: HashMap<String, u64>,
}

#[derive(Serialize, Deserialize)]
struct SidecarFile {
    format: u32,
    anchors: Sidecar,
    engine: EngineState,
}

/// One open document.
pub struct Engine {
    doc: WordDoc,
    anchors: Anchors,
    ids: IdAlloc,
    agent: String,
    now: Option<i64>,
    state: EngineState,
    /// Error-severity validation findings per code the document already had.
    baseline: HashMap<String, usize>,
    /// What reads of the current version reuse (behind a lock so reads take `&self`).
    cache: Mutex<ReadCache>,
}

/// Read-side state of the current version.
#[derive(Default)]
struct ReadCache {
    /// Style sheet and document-wide lookups; dropped whenever the document changes.
    projector: Option<Arc<Projector>>,
    /// The view model last handed out, kept current across transactions once a host asked for
    /// one, so op results can carry the full patch from it.
    vm: Option<Arc<ViewModel>>,
    /// A view model the read ops built (`list_changes`, `get_block`, …) while no host holds
    /// one; kept apart from `vm` so it does not switch op results to full patches.
    scratch: Option<Arc<ViewModel>>,
}

impl ReadCache {
    /// Forget everything read from the version before.
    fn clear(&mut self) {
        self.projector = None;
        self.scratch = None;
    }
}

/// Signatures of the read ops (the `usage` of their INVALID_OP errors, and what `help`
/// prints). `outline`, `read` and `find` stay first: the error paths index them.
const READ_USAGE: &[(&str, &str)] = &[
    ("outline", "outline {}"),
    ("read", "read {range?, mode?, budget?}"),
    ("find", "find {query, regex?, story?, mode?}"),
    ("get_block", "get_block {anchor}"),
    ("list_changes", "list_changes {change_set?, author?, status?}"),
    ("list_comments", "list_comments {open_only?}"),
    ("list_images", "list_images {}"),
    ("get_image", "get_image {image|asset}"),
    ("lint", "lint {}"),
    ("list_fields", "list_fields {}"),
    ("help", "help {name?}"),
];

/// What a transaction rolls back to.
struct Checkpoint {
    parts: Vec<(PartIdx, xml::Document)>,
    part_count: usize,
    anchors: Anchors,
    issued: usize,
    /// Saved package + sidecar, when an op may add a part.
    package: Option<(Vec<u8>, Sidecar)>,
}

impl Engine {
    /// Open a `.docx` / `.docm`. `sidecar` is what [`Engine::save`] returned (or a bare anchors
    /// sidecar); without one, anchors are minted fresh at version 0.
    pub fn open(bytes: Vec<u8>, sidecar: Option<&str>) -> Result<Engine> {
        let mut doc = WordDoc::open(bytes)?;
        let (anchors, state) = match sidecar {
            None => (Anchors::mint(&doc), EngineState::default()),
            Some(s) => {
                let v: Value = serde_json::from_str(s).map_err(|e| Error::Anchor(format!("sidecar: {e}")))?;
                if v.get("anchors").is_some() {
                    let f: SidecarFile =
                        serde_json::from_value(v).map_err(|e| Error::Anchor(format!("sidecar: {e}")))?;
                    if f.format != SIDECAR_FORMAT {
                        return Err(Error::Anchor(format!("sidecar: unsupported engine format {}", f.format)));
                    }
                    (Anchors::attach(&doc, &f.anchors)?, f.engine)
                } else {
                    (Anchors::attach(&doc, &Sidecar::from_json(s)?)?, EngineState::default())
                }
            }
        };
        while doc.version() < state.version {
            doc.commit();
        }
        let ids = IdAlloc::scan(&doc);
        let baseline = error_counts(&validate::check(&doc));
        Ok(Engine {
            doc,
            anchors,
            ids,
            agent: DEFAULT_AGENT.to_owned(),
            now: None,
            state,
            baseline,
            cache: Mutex::default(),
        })
    }

    /// The error envelope for a document [`Engine::open`] refused.
    pub fn open_error(e: &Error) -> Value {
        let (code, reason) = match e {
            Error::Encrypted(_) => ("PROTECTED", Some("encrypted")),
            Error::LimitExceeded { .. } | Error::Limit(_) => ("LIMIT", None),
            _ => ("BAD_PACKAGE", None),
        };
        let mut o = OpError::new(code, e.to_string());
        o.reason = reason.map(str::to_owned);
        if let Error::LimitExceeded { name, max, actual } = e {
            o.limit = Some(ops::LimitInfo { name: name.clone(), max: *max, actual: *actual });
        }
        envelope(&o, 0, None, None)
    }

    pub fn version(&self) -> u64 {
        self.doc.version()
    }

    /// Fix the clock revisions are stamped with (Unix seconds); `None` = the system clock.
    pub fn set_now(&mut self, unix_secs: Option<i64>) {
        self.now = unix_secs;
    }

    /// The default revision author (a transaction's `author` overrides it).
    pub fn set_agent(&mut self, author: &str) {
        self.agent = author.to_owned();
        // The view model labels the agent's revisions `AI`.
        let c = self.cache_mut();
        c.vm = None;
        c.scratch = None;
    }

    pub fn doc(&self) -> &WordDoc {
        &self.doc
    }

    pub fn anchors(&self) -> &Anchors {
        &self.anchors
    }

    pub fn change_sets(&self) -> &[ChangeSetRec] {
        &self.state.change_sets
    }

    /// Markup-mode projection lines of `handles` (spec/projection.md), rendering only those
    /// blocks. Unknown handles are skipped.
    pub fn render(&self, handles: &[String]) -> Vec<String> {
        self.lines(handles, View::Markup, &self.project_options())
    }

    /// Projection lines of `handles` in `view`, falling back to the minimal formatter if the
    /// projection fails.
    fn lines(&self, handles: &[String], view: View, options: &ProjectOptions) -> Vec<String> {
        let refs: Vec<&str> = handles.iter().map(String::as_str).collect();
        match self.projector().and_then(|p| p.block_lines(&self.doc, &self.anchors, &refs, view, options)) {
            Ok(lines) => lines,
            Err(_) => render_lines(&self.doc, &self.anchors, handles, &RenderOpts { show: None, agent: &self.agent }),
        }
    }

    // ------------------------------------------------------------------ reads

    /// The projection options of the current version: the agent author, the change-set log
    /// and the engine's version.
    pub fn project_options(&self) -> ProjectOptions {
        ProjectOptions {
            agent_author: self.agent.clone(),
            change_sets: self
                .state
                .change_sets
                .iter()
                .map(|c| ChangeSetInfo {
                    id: c.id.clone(),
                    label: c.label.clone(),
                    author: c.author.clone(),
                    turn: c.turn.clone(),
                    version: Some(c.version),
                    revisions: c.revisions.iter().map(|&r| i64::from(r)).collect(),
                    untracked: Vec::new(),
                    status: c.status().to_owned(),
                })
                .collect(),
            version: Some(self.version()),
            file_name: None,
        }
    }

    fn cache(&self) -> MutexGuard<'_, ReadCache> {
        self.cache.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn cache_mut(&mut self) -> &mut ReadCache {
        self.cache.get_mut().unwrap_or_else(|p| p.into_inner())
    }

    /// The projector of the current version, built on first use.
    fn projector(&self) -> Result<Arc<Projector>> {
        let mut c = self.cache();
        if let Some(p) = &c.projector {
            return Ok(p.clone());
        }
        let p = Arc::new(Projector::new(&self.doc, &self.anchors)?);
        c.projector = Some(p.clone());
        Ok(p)
    }

    /// A projection read (`read_document`); the request's `options` are replaced by the
    /// engine's ([`Engine::project_options`]).
    pub fn read_projection(&self, req: &ReadRequest) -> Result<ReadResult> {
        let req = ReadRequest { options: self.project_options(), ..req.clone() };
        self.projector()?.read(&self.doc, &self.anchors, &req)
    }

    /// Find matching blocks; the request's `options` are replaced by the engine's.
    pub fn find(&self, req: &FindRequest) -> Result<project::FindResult> {
        let req = FindRequest { options: self.project_options(), ..req.clone() };
        self.projector()?.find(&self.doc, &self.anchors, &req)
    }

    /// The view model of the current version (spec/schema/view-model.schema.json). Once a host
    /// has asked for one, the engine keeps it current and every op result's `patch` is the full
    /// patch from the previous view model; before that, a patch carries only `removes` and the
    /// changed change sets.
    pub fn view_model(&self) -> Result<Arc<ViewModel>> {
        if let Some(vm) = &self.cache().vm {
            return Ok(vm.clone());
        }
        let scratch = self.cache().scratch.take();
        let vm = match scratch {
            Some(vm) => vm,
            None => Arc::new(self.projector()?.view_model(&self.doc, &self.anchors, &self.project_options())?),
        };
        self.cache().vm = Some(vm.clone());
        Ok(vm)
    }

    /// The view model of the current version for a read op: the host's if it holds one, else
    /// one built for the reads of this version only.
    fn vm_now(&self) -> Result<Arc<ViewModel>> {
        {
            let c = self.cache();
            if let Some(vm) = c.vm.as_ref().or(c.scratch.as_ref()) {
                return Ok(vm.clone());
            }
        }
        let vm = Arc::new(self.projector()?.view_model(&self.doc, &self.anchors, &self.project_options())?);
        self.cache().scratch = Some(vm.clone());
        Ok(vm)
    }

    /// [`Engine::view_model`] as JSON, or the error envelope JSON.
    pub fn view_model_json(&self) -> String {
        match self.view_model() {
            Ok(vm) => serde_json::to_string(&*vm).expect("view model serializes"),
            Err(e) => envelope(&read_error(e, "view_model"), self.version(), None, Some("view_model")).to_string(),
        }
    }

    /// Run a read op (spec/schema/ops.schema.json `ReadOp`) given as JSON. Returns the
    /// ReadResult JSON or the error envelope JSON.
    pub fn read(&self, request: &str) -> String {
        match serde_json::from_str::<Value>(request) {
            Ok(v) => match self.read_op(&v) {
                Ok(r) | Err(r) => r.to_string(),
            },
            Err(e) => {
                let o = OpError {
                    usage: Some(READ_USAGE[1].1.to_owned()),
                    ..OpError::invalid("", format!("the request is not JSON: {e}"))
                };
                envelope(&o, self.version(), None, None).to_string()
            }
        }
    }

    /// Run a parsed read op: `Ok(ReadResult)` or `Err(error envelope)`. `outline`, `read`,
    /// `find`, `lint` and `help` answer in `text`; `get_block`, `list_changes`,
    /// `list_comments`, `list_images`, `get_image` and `list_fields` in `data`.
    pub fn read_op(&self, req: &Value) -> std::result::Result<Value, Value> {
        let version = self.version();
        let usage_of =
            |op: &str| READ_USAGE.iter().find(|(n, _)| *n == op).map_or(READ_USAGE[1].1, |(_, u)| u).to_owned();
        let bad = |op: Option<&str>, path: &str, msg: String| {
            let o = OpError { usage: Some(usage_of(op.unwrap_or("read"))), ..OpError::invalid(path, msg) };
            envelope(&o, version, None, op)
        };
        let Some(obj) = req.as_object() else {
            return Err(bad(None, "", "a read request is a JSON object {op, ...}".into()));
        };
        let Some(op) = obj.get("op").and_then(Value::as_str) else {
            return Err(bad(None, "/op", "op (outline, read, find, ...) is required".into()));
        };
        let keys: &[&str] = match op {
            "outline" | "list_images" | "lint" | "list_fields" => &["op", "spec_version"],
            "read" => &["op", "spec_version", "range", "mode", "budget"],
            "find" => &["op", "spec_version", "query", "regex", "story", "mode"],
            "get_block" => &["op", "spec_version", "anchor"],
            "list_changes" => &["op", "spec_version", "change_set", "author", "status"],
            "list_comments" => &["op", "spec_version", "open_only"],
            "get_image" => &["op", "spec_version", "image", "asset"],
            "help" => &["op", "spec_version", "name"],
            _ => return Err(bad(None, "/op", format!("{op:?} is not a read op"))),
        };
        for k in obj.keys() {
            if !keys.contains(&k.as_str()) {
                return Err(bad(Some(op), &format!("/{k}"), format!("{op} takes no {k:?}")));
            }
        }
        if let Some(sv) = obj.get("spec_version")
            && !sv.is_string()
        {
            return Err(bad(Some(op), "/spec_version", "spec_version is a version string".into()));
        }
        if !matches!(op, "outline" | "read" | "find") {
            let (text, data) = match self.read_more(op, obj) {
                Ok(reads::Answer::Text(t)) => (Value::String(t), Value::Null),
                Ok(reads::Answer::Data(d)) => (Value::Null, d),
                Err(e) => return Err(envelope(&e, version, None, Some(op))),
            };
            return Ok(json!({
                "spec_version": SPEC_VERSION,
                "ok": true,
                "version": version,
                "text": text,
                "data": data,
                "next": null,
            }));
        }
        let view = match obj.get("mode") {
            None | Some(Value::Null) => View::Accepted,
            Some(m) => match m.as_str() {
                Some("accepted") => View::Accepted,
                Some("markup") => View::Markup,
                _ => return Err(bad(Some(op), "/mode", "mode is accepted or markup".into())),
            },
        };
        let fail = |e: Error| envelope(&read_error(e, op), version, None, Some(op));
        let (text, next) = match op {
            "find" => {
                let query = match obj.get("query").and_then(Value::as_str) {
                    Some(q) if !q.is_empty() => q.to_owned(),
                    _ => return Err(bad(Some(op), "/query", "query (a non-empty string) is required".into())),
                };
                let regex = match obj.get("regex") {
                    None | Some(Value::Null) => false,
                    Some(Value::Bool(b)) => *b,
                    Some(_) => return Err(bad(Some(op), "/regex", "regex is true or false".into())),
                };
                let story = match obj.get("story") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(_) => return Err(bad(Some(op), "/story", "story is a story id (b, h1, fn2, ...)".into())),
                };
                let r = self
                    .find(&FindRequest { query: query.clone(), regex, story, view, ..FindRequest::default() })
                    .map_err(fail)?;
                (r.text(&query), None)
            }
            _ => {
                let range = match obj.get("range") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) if op == "read" => Some(s.clone()),
                    Some(_) => {
                        return Err(bad(
                            Some(op),
                            "/range",
                            "range is a selection string (b105, b105..b10h, t1.., ...)".into(),
                        ));
                    }
                };
                let budget = match obj.get("budget") {
                    None | Some(Value::Null) => None,
                    Some(b) => match b.as_u64() {
                        Some(n) if n >= project::MIN_BUDGET as u64 => Some(usize::try_from(n).unwrap_or(usize::MAX)),
                        _ => {
                            return Err(bad(
                                Some(op),
                                "/budget",
                                format!("budget is an integer of at least {}", project::MIN_BUDGET),
                            ));
                        }
                    },
                };
                let mode = if op == "outline" { ReadMode::Outline } else { ReadMode::Full };
                let r = self
                    .read_projection(&ReadRequest { view, range, budget, mode, ..ReadRequest::default() })
                    .map_err(fail)?;
                (r.text, r.next)
            }
        };
        Ok(json!({
            "spec_version": SPEC_VERSION,
            "ok": true,
            "version": version,
            "text": text,
            "data": null,
            "next": next,
        }))
    }

    /// An op result's `lint`: findings on the changed blocks, plus references the transaction
    /// broke by removing or moving their bookmark. Empty when the new version does not project.
    fn tx_lint(&self, changed: &[String], removed: &[String], bookmarks_before: &HashMap<String, String>) -> Value {
        Value::Array(self.tx_findings(changed, removed, bookmarks_before).iter().map(reads::Finding::to_json).collect())
    }

    /// The outline line of the current version, if it projects.
    fn outline_now(&self) -> Option<String> {
        self.projector().and_then(|p| p.outline_line(&self.doc, &self.anchors, &self.project_options())).ok()
    }

    /// The saved `.docx` bytes and the sidecar JSON to reopen it with.
    pub fn save(&mut self) -> Result<(Vec<u8>, String)> {
        let bytes = self.doc.save()?;
        self.cache_mut().clear();
        Ok((bytes, self.sidecar_json()))
    }

    fn sidecar_json(&self) -> String {
        let mut state = self.state.clone();
        state.version = self.doc.version();
        let f = SidecarFile { format: SIDECAR_FORMAT, anchors: self.anchors.sidecar(&self.doc), engine: state };
        serde_json::to_string(&f).expect("sidecar serializes")
    }

    /// Apply a transaction given as JSON. Returns the OpResult JSON, or the error envelope JSON.
    pub fn apply(&mut self, tx_json: &str) -> String {
        match serde_json::from_str::<Value>(tx_json) {
            Ok(tx) => match self.apply_tx(&tx) {
                Ok(v) | Err(v) => v.to_string(),
            },
            Err(e) => {
                let o = OpError::invalid("", format!("the transaction is not JSON: {e}"));
                envelope(&o, self.version(), None, None).to_string()
            }
        }
    }

    /// Apply a transaction and return the spec's §9 text form of the result or error.
    pub fn apply_text(&mut self, tx_json: &str) -> String {
        let tx: Value = match serde_json::from_str(tx_json) {
            Ok(v) => v,
            Err(e) => {
                let o = OpError::invalid("", format!("the transaction is not JSON: {e}"));
                return render_error_text(&envelope(&o, self.version(), None, None));
            }
        };
        let review = tx.get("ops").and_then(Value::as_array).is_some_and(|ops| {
            !ops.is_empty()
                && ops.iter().all(|o| matches!(o.get("op").and_then(Value::as_str), Some("accept" | "reject")))
        });
        match self.apply_tx(&tx) {
            Ok(r) => render_result_text(&r, review),
            Err(e) => render_error_text(&e),
        }
    }

    /// Apply a parsed transaction: `Ok(OpResult)` or `Err(error envelope)`.
    pub fn apply_tx(&mut self, tx: &Value) -> std::result::Result<Value, Value> {
        let version = self.version();
        let fail = |o: OpError, index: Option<usize>, op: Option<&str>| envelope(&o, version, index, op);

        // ---- shape
        let Some(obj) = tx.as_object() else {
            return Err(fail(OpError::invalid("", "a transaction is a JSON object {base_version, ops}"), None, None));
        };
        let ops_v = match obj.get("ops") {
            Some(Value::Array(a)) => a,
            _ => return Err(fail(OpError::invalid("/ops", "ops must be an array of ops"), None, None)),
        };
        if ops_v.len() > MAX_OPS {
            let o = OpError {
                limit: Some(ops::LimitInfo {
                    name: "ops_per_transaction".into(),
                    max: MAX_OPS as i64,
                    actual: ops_v.len() as i64,
                }),
                ..OpError::new("LIMIT", format!("{} ops in one transaction; the limit is {MAX_OPS}", ops_v.len()))
            };
            return Err(fail(o, None, None));
        }
        for k in obj.keys() {
            if !TX_KEYS.contains(&k.as_str()) {
                return Err(fail(
                    OpError::invalid(format!("/{k}"), format!("a transaction takes no {k:?}")),
                    None,
                    None,
                ));
            }
        }
        if ops_v.is_empty() {
            return Err(fail(OpError::invalid("/ops", "ops is empty"), None, None));
        }
        let mut names = Vec::with_capacity(ops_v.len());
        for (i, op) in ops_v.iter().enumerate() {
            match ops::check_shape(i, op) {
                Ok(spec) => names.push(spec.name),
                Err(e) => return Err(fail(e, Some(i), op.get("op").and_then(Value::as_str))),
            }
        }
        let Some(base) = obj.get("base_version").and_then(Value::as_u64) else {
            return Err(fail(
                OpError::invalid("/base_version", "base_version (the version you read) is required"),
                None,
                None,
            ));
        };
        let tx_mode = match obj.get("mode").map(|m| m.as_str()) {
            None => Mode::Track,
            Some(Some("track")) => Mode::Track,
            Some(Some("direct")) => Mode::Direct,
            Some(_) => return Err(fail(OpError::invalid("/mode", "mode is track or direct"), None, None)),
        };
        let markup = match obj.get("projection_mode").map(|m| m.as_str()) {
            None | Some(Some("accepted")) => false,
            Some(Some("markup")) => true,
            Some(_) => {
                return Err(fail(
                    OpError::invalid("/projection_mode", "projection_mode is accepted or markup"),
                    None,
                    None,
                ));
            }
        };
        let str_field = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_owned);
        let author = str_field("author").filter(|a| !a.is_empty()).unwrap_or_else(|| self.agent.clone());
        let label = str_field("label");
        let turn = str_field("turn");
        if base > version {
            let o = OpError::new(
                "BAD_VERSION",
                format!("base_version {base} is ahead of the document (version {version})"),
            );
            return Err(fail(o, None, None));
        }
        for (i, op) in ops_v.iter().enumerate() {
            if let Some(b) = op.get("base_version").and_then(Value::as_u64)
                && b > version
            {
                let o = OpError::new(
                    "BAD_VERSION",
                    format!("base_version {b} is ahead of the document (version {version})"),
                );
                return Err(fail(o, Some(i), Some(names[i])));
            }
        }

        // ---- apply
        let outline_before = self.outline_now();
        // Bookmarks before the edit: a reference that loses its target to this transaction is
        // linted even where the reference itself did not change.
        let bookmarks_before = self.projector().map(|p| p.bookmarks().clone()).unwrap_or_default();
        let may_add = names.iter().any(|n| handlers::MAY_ADD_PARTS.contains(n));
        let cp = match self.checkpoint(may_add) {
            Ok(cp) => cp,
            Err(e) => return Err(fail(OpError::new("INTERNAL", e.to_string()), None, None)),
        };
        // From here the document changes: nothing read before may be reused.
        self.cache_mut().clear();
        let cs_map: HashMap<String, Vec<u32>> =
            self.state.change_sets.iter().map(|c| (c.id.clone(), c.revisions.clone())).collect();
        let stamp = Stamp::new(author.clone(), self.now.unwrap_or_else(now_secs));
        let mut outcomes: Vec<EditOutcome> = Vec::new();
        let mut review: Vec<(bool, Vec<u32>)> = Vec::new();
        let (revisions, aliases, absorbed, warnings, failure) = {
            let mut tk = Tk::new(&mut self.ids, stamp);
            let mut failure: Option<(OpError, usize, Vec<String>)> = None;
            for (i, op) in ops_v.iter().enumerate() {
                let name = names[i];
                tk.mode = match op.get("mode").and_then(Value::as_str) {
                    Some("direct") => Mode::Direct,
                    Some("track") => Mode::Track,
                    _ => tx_mode,
                };
                if name == "set_track_changes" {
                    tk.mode = Mode::Direct;
                }
                let op_base = op.get("base_version").and_then(Value::as_u64).unwrap_or(base);
                if let Err((e, cur)) = guard(&self.doc, &self.anchors, &self.state.last_changed, i, name, op, op_base) {
                    failure = Some((e, i, cur));
                    break;
                }
                let mut cx = Cx {
                    doc: &mut self.doc,
                    anchors: &mut self.anchors,
                    tk: &mut tk,
                    index: i,
                    change_sets: &cs_map,
                    author: author.clone(),
                };
                match handlers::dispatch(&mut cx, name, op) {
                    Ok(out) => {
                        if matches!(name, "accept" | "reject") {
                            review.push((name == "accept", out.revisions.clone()));
                        }
                        outcomes.push(out);
                    }
                    Err(mut e) => {
                        if e.anchor.is_none()
                            && let Some(h) = target_handle(name, op)
                            && (e.wants_current || matches!(e.code, "NOT_FOUND" | "AMBIGUOUS" | "IN_FIELD"))
                        {
                            e.anchor = Some(h.to_owned());
                        }
                        let cur = if e.wants_current { e.anchor.iter().cloned().collect() } else { Vec::new() };
                        failure = Some((e, i, cur));
                        break;
                    }
                }
            }
            (tk.revisions, tk.aliases, tk.absorbed, tk.warnings, failure)
        };

        if let Some((e, i, cur)) = failure {
            self.rollback(cp);
            return Err(self.error_with_current(e, Some(i), Some(names[i]), &cur));
        }

        // ---- validate
        let report = validate::check(&self.doc);
        let after = error_counts(&report);
        let grew: Vec<&validate::Finding> = report
            .errors()
            .filter(|f| after.get(&f.code).copied().unwrap_or(0) > self.baseline.get(&f.code).copied().unwrap_or(0))
            .collect();
        if let Some(f) = grew.first() {
            let o = OpError {
                invariant: Some(invariant_id(&f.code).to_owned()),
                ..OpError::new(
                    "INVARIANT",
                    format!("the edit would break the document ({}: {}); nothing was changed", f.code, f.message),
                )
            };
            self.rollback(cp);
            return Err(fail(o, None, None));
        }

        // ---- commit
        let new_version = self.doc.commit();
        self.baseline = after;
        let present = present_ids(&self.doc);
        let mut own: Vec<u32> = revisions.into_iter().filter(|id| present.contains(id)).collect();
        own.sort_unstable();
        own.dedup();
        // Earlier change sets: a split copy of one of their revisions joins them; a same-author
        // insertion rewritten in place that is now gone (its text all deleted) leaves them.
        let mut edited_cs: Vec<usize> = Vec::new();
        for (i, cs) in self.state.change_sets.iter_mut().enumerate() {
            let before = cs.revisions.clone();
            // Copies of copies (a wrapper split twice) follow the chain.
            let mut members: HashSet<u32> = cs
                .revisions
                .iter()
                .copied()
                .filter(|id| !cs.accepted.contains(id) && !cs.rejected.contains(id))
                .collect();
            for (orig, copy) in &aliases {
                if members.contains(orig) {
                    members.insert(*copy);
                    if present.contains(copy) && !cs.revisions.contains(copy) {
                        cs.revisions.push(*copy);
                    }
                }
            }
            cs.revisions.retain(|id| !(absorbed.contains(id) && !present.contains(id)));
            if cs.revisions != before || absorbed.iter().any(|id| before.contains(id)) {
                edited_cs.push(i);
            }
        }
        for (accepted, ids) in &review {
            for cs in &mut self.state.change_sets {
                for id in ids {
                    if cs.revisions.contains(id) && !cs.accepted.contains(id) && !cs.rejected.contains(id) {
                        if *accepted { cs.accepted.push(*id) } else { cs.rejected.push(*id) }
                    }
                }
            }
        }
        let cs_id = if own.is_empty() {
            None
        } else {
            let id = format!("cs_{}", base36(self.state.next_cs + 1));
            self.state.next_cs += 1;
            self.state.change_sets.push(ChangeSetRec {
                id: id.clone(),
                label: label.clone(),
                author: author.clone(),
                turn: turn.clone(),
                version: new_version,
                revisions: own.clone(),
                accepted: Vec::new(),
                rejected: Vec::new(),
            });
            Some(id)
        };

        let mut changed: Vec<String> = Vec::new();
        let mut removed: Vec<String> = Vec::new();
        for o in &outcomes {
            for h in o.changed.iter().chain(&o.inserted) {
                if !changed.contains(h) {
                    changed.push(h.clone());
                }
            }
            for h in &o.deleted {
                if !removed.contains(h) {
                    removed.push(h.clone());
                }
            }
        }
        changed.retain(|h| !removed.contains(h) || self.anchors.resolve(&self.doc, h).is_some());
        removed.retain(|h| self.anchors.resolve(&self.doc, h).is_none());
        for h in changed.iter().chain(&removed) {
            self.state.last_changed.insert(h.clone(), new_version);
        }
        let changed = self.in_document_order(changed);
        // The lines as the next read shows them; in the accepted view the transaction's own
        // author is the agent, so its open revisions keep their markup.
        let line_opts = ProjectOptions { agent_author: author.clone(), ..self.project_options() };
        let lines = self.lines(&changed, if markup { View::Markup } else { View::Accepted }, &line_opts);
        let outline_after = self.outline_now();
        let outline = if outline_after != outline_before { outline_after } else { None };
        let touched: Vec<&ChangeSetRec> = self
            .state
            .change_sets
            .iter()
            .enumerate()
            .filter(|(i, c)| {
                Some(&c.id) == cs_id.as_ref()
                    || edited_cs.contains(i)
                    || review.iter().any(|(_, ids)| ids.iter().any(|id| c.revisions.contains(id)))
            })
            .map(|(_, c)| c)
            .collect();
        let mut patch = json!({
            "spec_version": SPEC_VERSION,
            "base_version": base,
            "version": new_version,
            "removes": removed,
            "moves": [],
            "upserts": [],
        });
        if !touched.is_empty() {
            patch["changeSets"] = Value::Array(touched.iter().map(|c| c.to_json()).collect());
        }
        // A host that holds a view model gets the full patch from it.
        let before_vm = self.cache_mut().vm.take();
        if let Some(before_vm) = before_vm
            && let Ok(after_vm) = self.view_model()
        {
            let forced: Vec<&str> = changed.iter().map(String::as_str).collect();
            let full = project::patch(&before_vm, &after_vm, &forced);
            patch = serde_json::to_value(&full).unwrap_or(patch);
        }
        let lint = self.tx_lint(&changed, &removed, &bookmarks_before);
        Ok(json!({
            "spec_version": SPEC_VERSION,
            "ok": true,
            "base_version": base,
            "version": new_version,
            "change_set": cs_id,
            "changed": lines,
            "removed": removed,
            "outline": outline,
            "patch": patch,
            "lint": lint,
            "warnings": warnings,
        }))
    }

    // ------------------------------------------------------------------ rollback

    fn checkpoint(&mut self, with_package: bool) -> Result<Checkpoint> {
        let package = if with_package {
            let bytes = self.doc.save()?;
            Some((bytes, self.anchors.sidecar(&self.doc)))
        } else {
            None
        };
        let parts: Vec<(PartIdx, xml::Document)> =
            self.doc.part_indices().map(|i| (i, self.doc.part(i).clone())).collect();
        Ok(Checkpoint {
            part_count: parts.len(),
            parts,
            anchors: self.anchors.clone(),
            issued: self.ids.issued().len(),
            package,
        })
    }

    fn rollback(&mut self, cp: Checkpoint) {
        self.ids.truncate(cp.issued);
        self.cache_mut().clear();
        let grew = self.doc.part_indices().count() != cp.part_count;
        if grew && let Some((bytes, sidecar)) = cp.package {
            let version = self.doc.version();
            if let Ok(mut doc) = WordDoc::open(bytes) {
                while doc.version() < version {
                    doc.commit();
                }
                if let Ok(anchors) = Anchors::attach(&doc, &sidecar) {
                    self.doc = doc;
                    self.anchors = anchors;
                    return;
                }
            }
        }
        for (i, d) in cp.parts {
            *self.doc.part_mut(i) = d;
        }
        self.anchors = cp.anchors;
    }

    // ------------------------------------------------------------------ helpers

    fn error_with_current(&self, mut e: OpError, index: Option<usize>, op: Option<&str>, current: &[String]) -> Value {
        if !current.is_empty() {
            let lines = self.render(current);
            if !lines.is_empty() {
                e.current = Some(lines);
            }
        }
        if e.code == "STALE_ANCHOR" && e.current.is_none() {
            // The block is gone: there is no fresh line to show.
            e.code = "NOT_FOUND";
            e.what = Some("anchor");
        }
        envelope(&e, self.version(), index, op)
    }

    /// Sort handles by their position in the document (stories in order, blocks in order;
    /// table / row / cell handles at their first paragraph).
    fn in_document_order(&self, handles: Vec<String>) -> Vec<String> {
        let mut pos: HashMap<&str, usize> = HashMap::new();
        let mut n = 0usize;
        for s in 0..self.doc.stories().len() {
            for (h, _) in self.anchors.handles(s) {
                pos.insert(h, n);
                n += 1;
            }
        }
        let key = |h: &String| -> (usize, usize) {
            if let Some(&p) = pos.get(h.as_str()) {
                return (p, 1);
            }
            let first = match self.anchors.resolve(&self.doc, h) {
                Some(Target::Table { table }) | Some(Target::Row { table, .. }) | Some(Target::Cell { table, .. }) => {
                    let d = self.doc.part(table.part);
                    ops::paras_in(d, table.node)
                        .first()
                        .and_then(|&p| self.anchors.handle_of(crate::wml::Loc::new(table.part, p)))
                        .and_then(|ph| pos.get(ph).copied())
                }
                _ => None,
            };
            (first.unwrap_or(usize::MAX), 0)
        };
        let mut v: Vec<(usize, usize, usize, String)> = handles
            .into_iter()
            .enumerate()
            .map(|(i, h)| {
                let (a, b) = key(&h);
                (a, b, i, h)
            })
            .collect();
        v.sort();
        v.into_iter().map(|(_, _, _, h)| h).collect()
    }
}

/// The handle an op targets (for `expect`, staleness and error anchors).
fn target_handle<'a>(name: &str, op: &'a Value) -> Option<&'a str> {
    let key = ops::target_key(name)?;
    op.get(key).or_else(|| op.get("before")).and_then(Value::as_str)
}

/// `expect` and staleness for one op. `Err` carries the error and the handles to render as
/// `current`.
#[allow(clippy::result_large_err)] // cold failure path, see `ops`
fn guard(
    doc: &WordDoc,
    anchors: &Anchors,
    last_changed: &HashMap<String, u64>,
    index: usize,
    name: &str,
    op: &Value,
    base: u64,
) -> std::result::Result<(), (OpError, Vec<String>)> {
    let Some(handle) = target_handle(name, op) else { return Ok(()) };
    let key = ops::target_key(name).unwrap_or("anchor");
    // A target that does not resolve is the op's own NOT_FOUND.
    let Ok(r) = ops::resolve(doc, anchors, index, key, handle) else { return Ok(()) };
    let current = || if r.table.is_some() { r.watch.clone() } else { vec![handle.to_owned()] };
    if let Some(e) = op.get("expect") {
        let text = ops::target_text(doc, &r);
        let ok = match (e.get("text").and_then(Value::as_str), e.get("hash").and_then(Value::as_str)) {
            (Some(t), _) => t == text,
            (None, Some(h)) => {
                h.strip_prefix("sha256:").is_some_and(|hex| sha256_hex(text.as_bytes()).starts_with(hex))
            }
            _ => true,
        };
        if ok {
            return Ok(());
        }
        let o = OpError {
            anchor: Some(handle.to_owned()),
            ..OpError::new("STALE_ANCHOR", format!("{handle} no longer matches expect; current text below."))
        };
        return Err((o, current()));
    }
    let stale = r.watch.iter().find(|h| last_changed.get(h.as_str()).is_some_and(|&v| v > base));
    if let Some(h) = stale {
        let o = OpError {
            anchor: Some(handle.to_owned()),
            ..OpError::new("STALE_ANCHOR", format!("{h} changed after base_version {base}; current text below."))
        };
        return Err((o, current()));
    }
    Ok(())
}

/// The error envelope JSON.
/// The error of a read op the projection refused.
fn read_error(e: Error, op: &str) -> OpError {
    let msg = e.to_string();
    match e {
        Error::Anchor(m) if m.starts_with("unknown comment") => OpError::not_found("comment", msg),
        Error::Anchor(m) if m.starts_with("unknown") => OpError::not_found("anchor", msg),
        Error::Anchor(_) => {
            let path = if op == "find" { "/story" } else { "/range" };
            OpError {
                usage: READ_USAGE.iter().find(|(n, _)| *n == op).map(|(_, u)| (*u).to_owned()),
                ..OpError::invalid(path, msg)
            }
        }
        Error::Other(_) if op == "find" => {
            OpError { usage: Some(READ_USAGE[2].1.to_owned()), ..OpError::invalid("/query", msg) }
        }
        Error::NotFound(_) => OpError::not_found("asset", msg),
        Error::Limit(m) => OpError::new("LIMIT", m),
        _ => OpError::new("INTERNAL", msg),
    }
}

fn envelope(e: &OpError, version: u64, index: Option<usize>, op: Option<&str>) -> Value {
    let mut err = json!({
        "code": e.code,
        "message": e.message,
        "retryable": e.retryable(),
        "version": version,
        "op_index": index,
        "op": op,
    });
    let o = err.as_object_mut().expect("object");
    let mut put = |k: &str, v: Value| {
        o.insert(k.to_owned(), v);
    };
    if let Some(a) = &e.anchor {
        put("anchor", json!(a));
    }
    if let Some(c) = &e.current {
        put("current", json!(c));
    }
    if let Some(c) = &e.candidates {
        put("candidates", serde_json::to_value(c).unwrap_or(Value::Null));
    }
    if let Some(w) = e.what {
        put("what", json!(w));
    }
    if let Some(r) = &e.reason {
        put("reason", json!(r));
    }
    if let Some(l) = &e.limit {
        put("limit", serde_json::to_value(l).unwrap_or(Value::Null));
    }
    if e.code == "INVALID_OP" {
        // spec/errors.md: INVALID_OP always carries `path` and `usage`. Errors raised below the
        // op layer know neither; default to the op (or the whole request) and its signature.
        let path = e.path.clone().unwrap_or_else(|| index.map_or_else(String::new, |i| format!("/ops/{i}")));
        let usage = e.usage.clone().unwrap_or_else(|| op.map_or_else(|| TX_USAGE.to_owned(), ops::usage));
        put("path", json!(path));
        put("usage", json!(usage));
    } else {
        if let Some(p) = &e.path {
            put("path", json!(p));
        }
        if let Some(u) = &e.usage {
            put("usage", json!(u));
        }
    }
    if let Some(i) = &e.invariant {
        put("invariant", json!(i));
    }
    json!({ "spec_version": SPEC_VERSION, "ok": false, "error": err })
}

fn error_counts(r: &validate::Report) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for f in r.errors() {
        *m.entry(f.code.clone()).or_insert(0) += 1;
    }
    m
}

/// The spec/errors.md invariant id for a validation finding code.
pub fn invariant_id(code: &str) -> &'static str {
    if code.contains("comment") {
        return "comment_ranges_balanced";
    }
    match code {
        "id.para_id_invalid" | "id.text_id_invalid" | "id.long_hex_invalid" => "para_id_range",
        "order.sectpr_not_last" => "sectpr_last",
        "package.missing_content_type" => "content_types",
        "package.missing_target" | "package.duplicate_rel_id" => "rel_targets",
        "table.grid_mismatch" | "table.no_grid" => "grid_spans",
        "xml.ignorable_undeclared" => "ignorable_namespaces",
        c if c.starts_with("id.") => "unique_ids",
        c if c.starts_with("order.") || c.starts_with("value.") => "schema_order",
        c if c.starts_with("rel.") => "rel_targets",
        c if c.starts_with("table.cell_") => "cell_ends_with_p",
        c if c.starts_with("track.") => "revision_nesting",
        c if c.starts_with("numbering.") => "numbering_order",
        c if c.starts_with("text.") => "text_space",
        _ => "well_formed",
    }
}

/// Every `w:id` in the document (revision and annotation ids).
fn present_ids(doc: &WordDoc) -> HashSet<u32> {
    let mut out = HashSet::new();
    for idx in doc.part_indices() {
        let d = doc.part(idx);
        for n in d.descendants(d.root()) {
            if d.is_element_node(n)
                && d.namespace(n) == Some(ns::W)
                && d.attr_count(n) > 0
                && let Some(id) = d.attr_ns(n, ns::W, "id").and_then(|v| v.trim().parse::<u32>().ok())
            {
                out.insert(id);
            }
        }
    }
    out
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_owned();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("ascii")
}

/// SHA-256 as lowercase hex (FIPS 180-4), for `expect.hash`.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, b) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let long = vec![b'a'; 1000];
        assert_eq!(sha256_hex(&long), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
    }

    #[test]
    fn base36_ids() {
        assert_eq!(base36(1), "1");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
    }

    #[test]
    fn invariant_ids() {
        assert_eq!(invariant_id("id.para_id_duplicate"), "unique_ids");
        assert_eq!(invariant_id("id.para_id_invalid"), "para_id_range");
        assert_eq!(invariant_id("order.sectpr_not_last"), "sectpr_last");
        assert_eq!(invariant_id("table.cell_not_ending_in_p"), "cell_ends_with_p");
        assert_eq!(invariant_id("xml.malformed"), "well_formed");
    }
}
