//! Post-edit validation (DESIGN §10): cheap structural checks run after every transaction.
//!
//! [`check`] walks every loaded part once and reports [`Finding`]s. Errors are things Word or
//! LibreOffice refuse, repair or silently misread (bad element order, dangling ids, text inside
//! a deletion, a missing part); warnings are oddities real documents carry and readers accept
//! (unpaired bookmarks, a row that does not fill the grid). An edit must leave zero errors.
//!
//! What is checked:
//! - package: every part has a content type, every internal relationship target exists, rel ids
//!   are unique, the main document part is there (the last via [`check_bytes`]);
//! - XML: every part (or, after an edit, every modified part) is well formed, `mc:Ignorable`
//!   prefixes are declared;
//! - ids: revision ids, `w14:paraId`, comment ids, bookmark ids and `wp:docPr` ids are unique;
//!   `paraId` / `textId` / `durableId` are 8 hex digits in `1..0x80000000`; every `r:*`
//!   attribute names a relationship of its part;
//! - order: `pPr` / `rPr` / `tcPr` / `tblPr` / `trPr` first in their parent, `sectPr` last in
//!   the body, children of `pPr` / `rPr` / `tcPr` / `tblPr` / `trPr` / `sectPr` in schema order;
//!   children of `w:settings`, `w:styles`, `w:docDefaults`, `w:style` and `w:tblStylePr` in schema
//!   order (warnings);
//! - values: `jc`, `vAlign`, `vMerge` enums and on/off toggles;
//! - tracked changes: no `w:t` in a deletion, no `w:ins` in a `w:ins` (or `del` in `del`),
//!   `w:delText` only inside a deletion;
//! - ranges: bookmarks and comment ranges paired, comment references resolve, comment
//!   extension parts point at real comments;
//! - tables: every cell ends with a paragraph, rows fill the grid;
//! - numbering: `abstractNum` before `num`, `num` / `numPr` references resolve;
//! - text: `w:t` with leading or trailing spaces carries `xml:space="preserve"`.
//!
//! `mc:Fallback` (and non-first `mc:Choice`) branches are skipped: they duplicate ids by design.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde::Serialize;

use crate::anchors::Anchors;
use crate::edit::{is_ignored_branch, walk};
use crate::opc::{self, Package};
use crate::ops::handlers::SETTINGS_ORDER;
use crate::wml::{Loc, PartIdx, PartKind, WordDoc};
use crate::xml::{Document, NodeId, ns};

/// Inserts `k -> v` unless `k` is already there; returns the key back when it was a duplicate.
fn insert_new<V>(map: &mut HashMap<String, V>, k: String, v: V) -> Option<String> {
    match map.entry(k) {
        std::collections::hash_map::Entry::Occupied(e) => Some(e.key().clone()),
        std::collections::hash_map::Entry::Vacant(e) => {
            e.insert(v);
            None
        }
    }
}

/// How bad a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Word or LibreOffice would refuse, repair or misread the document.
    Error,
    /// Unusual but tolerated by readers.
    Warning,
}

/// One problem found by [`check`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Stable dotted code, e.g. `order.ppr_child`, `id.para_id_duplicate`.
    pub code: String,
    /// Part name (`word/document.xml`); `""` for package-level findings.
    pub part: String,
    /// XPath-like location (`/w:document[1]/w:body[1]/w:p[3]`), when the finding is on a node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Handle of the enclosing paragraph or table, when [`check_with`] got anchors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    pub message: String,
    pub severity: Severity,
}

/// Every finding of one check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.severity == Severity::Error)
    }
    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.severity == Severity::Warning)
    }
    /// No errors (warnings allowed).
    pub fn is_ok(&self) -> bool {
        self.errors().next().is_none()
    }
    /// Is there a finding with this code?
    pub fn has(&self, code: &str) -> bool {
        self.findings.iter().any(|f| f.code == code)
    }
}

/// Check a loaded document (package checks run when it was opened from a package).
pub fn check(doc: &WordDoc) -> Report {
    check_with(doc, None)
}

/// [`check`], filling [`Finding::anchor`] from `anchors`.
pub fn check_with(doc: &WordDoc, anchors: Option<&Anchors>) -> Report {
    let mut c = Checker::new(doc, anchors);
    for (name, bytes) in doc.modified_parts() {
        if let Err(e) = Document::parse(&bytes) {
            c.push_part(Severity::Error, "xml.malformed", &name, format!("modified part is not well formed: {e}"));
        }
    }
    if let Some(pkg) = doc.package() {
        package_checks(&mut c, pkg);
    }
    c.run();
    c.report()
}

/// Check raw `.docx` bytes: zip and package structure, every XML part well formed, then
/// Content types Word accepts for the main document part.
const MAIN_CONTENT_TYPES: &[&str] = &[
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
    "application/vnd.ms-word.document.macroEnabled.main+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml",
    "application/vnd.ms-word.template.macroEnabledTemplate.main+xml",
];

/// everything [`check`] does.
pub fn check_bytes(bytes: &[u8]) -> Report {
    let mut out = Report::default();
    let push = |out: &mut Report, code: &str, part: &str, message: String| {
        out.findings.push(Finding {
            code: code.into(),
            part: part.into(),
            path: None,
            anchor: None,
            message,
            severity: Severity::Error,
        })
    };
    let pkg = match Package::open(bytes.to_vec(), &opc::Limits::default()) {
        Ok(p) => p,
        Err(e) => {
            let code = match e {
                crate::Error::Encrypted(_) => "package.encrypted",
                _ if !bytes.starts_with(b"PK") => "package.not_a_zip",
                _ => "package.invalid",
            };
            push(&mut out, code, "", e.to_string());
            return out;
        }
    };
    let main = pkg.rels_of("").ok().and_then(|rels| {
        rels.iter().find(|r| !r.external && r.rel_type.ends_with("/officeDocument")).and_then(|r| pkg.resolve("", r))
    });
    match &main {
        Some(m) if pkg.contains(m) => match pkg.content_type_of(m) {
            Some(ct) if MAIN_CONTENT_TYPES.contains(&ct) => {}
            ct => push(
                &mut out,
                "package.word_document_missing",
                m,
                format!("main part {m} has content type {}, not a WordprocessingML document", ct.unwrap_or("(none)")),
            ),
        },
        Some(m) => {
            push(&mut out, "package.word_document_missing", m, format!("main document part {m} is not in the package"))
        }
        None => push(&mut out, "package.word_document_missing", "", "no officeDocument relationship".into()),
    }
    let names: Vec<String> = pkg.names().map(str::to_owned).collect();
    for name in &names {
        let lower = name.to_ascii_lowercase();
        if !(lower.ends_with(".xml") || lower.ends_with(".rels")) {
            continue;
        }
        match pkg.part(name) {
            Ok(b) => {
                if let Err(e) = Document::parse(&b) {
                    push(&mut out, "xml.malformed", name, format!("not well formed: {e}"));
                }
            }
            Err(e) => push(&mut out, "package.invalid", name, format!("unreadable part: {e}")),
        }
    }
    if !out.findings.is_empty() {
        return out;
    }
    let doc = match WordDoc::load(&pkg) {
        Ok(d) => d,
        Err(e) => {
            push(&mut out, "package.invalid", "", format!("cannot load the document: {e}"));
            return out;
        }
    };
    let mut c = Checker::new(&doc, None);
    package_checks(&mut c, &pkg);
    c.run();
    c.report()
}

// ---------------------------------------------------------------------------------------------
// Package

fn package_checks(c: &mut Checker<'_>, pkg: &Package) {
    let names: Vec<&str> = pkg.names().collect();
    for name in &names {
        if name.ends_with('/') || name.eq_ignore_ascii_case("[Content_Types].xml") {
            continue;
        }
        if pkg.content_type_of(name).is_none() {
            let main = c.doc.part_name(c.doc.main_part()).eq_ignore_ascii_case(name);
            let what = if main { "the main document part" } else { "part" };
            c.push_part(
                Severity::Error,
                "package.missing_content_type",
                name,
                format!("{what} {name} has no content type"),
            );
        }
    }
    for name in &names {
        if !name.to_ascii_lowercase().ends_with(".rels") {
            continue;
        }
        let Some(source) = opc::source_of_rels(name) else { continue };
        let rels = match pkg.rels_of(&source) {
            Ok(r) => r,
            Err(e) => {
                c.push_part(Severity::Error, "package.invalid", name, format!("unreadable relationships: {e}"));
                continue;
            }
        };
        let mut seen = HashSet::new();
        for rel in &rels {
            if !seen.insert(rel.id.as_str()) {
                c.push_part(
                    Severity::Error,
                    "package.duplicate_rel_id",
                    name,
                    format!("relationship id {} is used twice", rel.id),
                );
            }
            if rel.external {
                continue;
            }
            match pkg.resolve(&source, rel) {
                Some(t) if pkg.contains(&t) => {}
                Some(t) => c.push_part(
                    Severity::Error,
                    "package.missing_target",
                    name,
                    format!("relationship {} points at {t}, which is not in the package", rel.id),
                ),
                None => c.push_part(
                    Severity::Error,
                    "package.missing_target",
                    name,
                    format!("relationship {} has an unusable target {:?}", rel.id, rel.target),
                ),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Schema tables

/// Children of `pPr` (CT_PPr), `rPr` (CT_ParaRPr ⊇ CT_RPr), `tcPr`, `tblPr`, `sectPr` in
/// schema order. `a|b` = same rank (any order among them). `trPr` is special-cased.
const PPR: &[&str] = &[
    "pStyle",
    "keepNext",
    "keepLines",
    "pageBreakBefore",
    "framePr",
    "widowControl",
    "numPr",
    "suppressLineNumbers",
    "pBdr",
    "shd",
    "tabs",
    "suppressAutoHyphens",
    "kinsoku",
    "wordWrap",
    "overflowPunct",
    "topLinePunct",
    "autoSpaceDE",
    "autoSpaceDN",
    "bidi",
    "adjustRightInd",
    "snapToGrid",
    "spacing",
    "ind",
    "contextualSpacing",
    "mirrorIndents",
    "suppressOverlap",
    "jc",
    "textDirection",
    "textAlignment",
    "textboxTightWrap",
    "outlineLvl",
    "divId",
    "cnfStyle",
    "rPr",
    "sectPr",
    "pPrChange",
];
const RPR: &[&str] = &[
    "ins",
    "del",
    "moveFrom",
    "moveTo",
    "rStyle",
    "rFonts",
    "b",
    "bCs",
    "i",
    "iCs",
    "caps",
    "smallCaps",
    "strike",
    "dstrike",
    "outline",
    "shadow",
    "emboss",
    "imprint",
    "noProof",
    "snapToGrid",
    "vanish",
    "webHidden",
    "color",
    "spacing",
    "w",
    "kern",
    "position",
    "sz",
    "szCs",
    "highlight",
    "u",
    "effect",
    "bdr",
    "shd",
    "fitText",
    "vertAlign",
    "rtl",
    "cs",
    "em",
    "lang",
    "eastAsianLayout",
    "specVanish",
    "oMath",
    "rPrChange",
];
const TCPR: &[&str] = &[
    "cnfStyle",
    "tcW",
    "gridSpan",
    "hMerge",
    "vMerge",
    "tcBorders",
    "shd",
    "noWrap",
    "tcMar",
    "textDirection",
    "tcFitText",
    "vAlign",
    "hideMark",
    "headers",
    "cellIns",
    "cellDel",
    "cellMerge",
    "tcPrChange",
];
const TBLPR: &[&str] = &[
    "tblStyle",
    "tblpPr",
    "tblOverlap",
    "bidiVisual",
    "tblStyleRowBandSize",
    "tblStyleColBandSize",
    "tblW",
    "jc",
    "tblCellSpacing",
    "tblInd",
    "tblBorders",
    "shd",
    "tblLayout",
    "tblCellMar",
    "tblLook",
    "tblCaption",
    "tblDescription",
    "tblPrChange",
];
const SECTPR: &[&str] = &[
    "headerReference|footerReference",
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
const TRPR: &[&str] = &[
    "cnfStyle|divId|gridBefore|gridAfter|wBefore|wAfter|cantSplit|trHeight|tblHeader|tblCellSpacing|jc|hidden",
    "ins|del",
    "trPrChange",
];
/// Children of `w:styles`, `w:docDefaults`, `w:style` (CT_Style) and `w:tblStylePr`.
const STYLES: &[&str] = &["docDefaults", "latentStyles", "style"];
const DOC_DEFAULTS: &[&str] = &["rPrDefault", "pPrDefault"];
const STYLE: &[&str] = &[
    "name",
    "aliases",
    "basedOn",
    "next",
    "link",
    "autoRedefine",
    "hidden",
    "uiPriority",
    "semiHidden",
    "unhideWhenUsed",
    "qFormat",
    "locked",
    "personal",
    "personalCompose",
    "personalReply",
    "rsid",
    "pPr",
    "rPr",
    "tblPr",
    "trPr",
    "tcPr",
    "tblStylePr",
];
const TBL_STYLE_PR: &[&str] = &["pPr", "rPr", "tblPr", "trPr", "tcPr"];
/// Part-level parents: order is a warning until it is calibrated on more real documents, and
/// repeats are not checked (`w:style`, `w:tblStylePr`, `w:activeWritingStyle`, ... repeat by
/// design).
const LENIENT: &[&str] = &["settings", "styles", "docDefaults", "style", "tblStylePr"];

/// (parent local name, child local name) → rank.
fn order_table() -> &'static HashMap<(&'static str, &'static str), usize> {
    static T: OnceLock<HashMap<(&'static str, &'static str), usize>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m = HashMap::new();
        for (parent, list) in [
            ("pPr", PPR),
            ("rPr", RPR),
            ("tcPr", TCPR),
            ("tblPr", TBLPR),
            ("sectPr", SECTPR),
            ("trPr", TRPR),
            ("settings", SETTINGS_ORDER),
            ("styles", STYLES),
            ("docDefaults", DOC_DEFAULTS),
            ("style", STYLE),
            ("tblStylePr", TBL_STYLE_PR),
        ] {
            for (rank, item) in list.iter().enumerate() {
                for name in item.split('|') {
                    m.insert((parent, name), rank);
                }
            }
        }
        m
    })
}

const ST_JC: &[&str] = &[
    "start",
    "center",
    "end",
    "both",
    "mediumKashida",
    "distribute",
    "numTab",
    "highKashida",
    "lowKashida",
    "thaiDistribute",
    "left",
    "right",
];
const ST_JC_TABLE: &[&str] = &["center", "end", "left", "right", "start"];
const ST_VALIGN: &[&str] = &["top", "center", "both", "bottom"];
const ST_ONOFF: &[&str] = &["true", "false", "on", "off", "0", "1"];
const TOGGLES: &[&str] = &[
    "b",
    "bCs",
    "i",
    "iCs",
    "caps",
    "smallCaps",
    "strike",
    "dstrike",
    "outline",
    "shadow",
    "emboss",
    "imprint",
    "noProof",
    "snapToGrid",
    "vanish",
    "webHidden",
    "rtl",
    "cs",
    "specVanish",
    "oMath",
    "keepNext",
    "keepLines",
    "pageBreakBefore",
    "widowControl",
    "suppressLineNumbers",
    "suppressAutoHyphens",
    "kinsoku",
    "wordWrap",
    "overflowPunct",
    "topLinePunct",
    "autoSpaceDE",
    "autoSpaceDN",
    "bidi",
    "adjustRightInd",
    "contextualSpacing",
    "mirrorIndents",
    "suppressOverlap",
];
/// Elements whose `w:id` lives in the revision id space.
const REVISIONS: &[&str] = &[
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
    "tblGridChange",
    "tblPrExChange",
    "numberingChange",
    "cellIns",
    "cellDel",
    "cellMerge",
];
/// Range markup that may follow the last paragraph of a cell.
const RANGE_MARKUP: &[&str] = &[
    "bookmarkStart",
    "bookmarkEnd",
    "commentRangeStart",
    "commentRangeEnd",
    "moveFromRangeStart",
    "moveFromRangeEnd",
    "moveToRangeStart",
    "moveToRangeEnd",
    "permStart",
    "permEnd",
    "proofErr",
    "customXmlInsRangeStart",
    "customXmlInsRangeEnd",
    "customXmlDelRangeStart",
    "customXmlDelRangeEnd",
    "customXmlMoveFromRangeStart",
    "customXmlMoveFromRangeEnd",
    "customXmlMoveToRangeStart",
    "customXmlMoveToRangeEnd",
];

/// `ST_LongHexNumber` as Word uses it for paraId / textId / durableId: 8 hex digits, value in
/// `1..0x80000000`.
fn long_hex(s: &str) -> Option<u32> {
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    (v > 0 && v < 0x8000_0000).then_some(v)
}

// ---------------------------------------------------------------------------------------------
// The walk

/// Qualified attribute names for one part, so hot lookups avoid namespace resolution.
struct Q {
    w: Option<String>,
    val: Option<String>,
    id: Option<String>,
    para_id: Option<String>,
    text_id: Option<String>,
}

impl Q {
    fn new(d: &Document) -> Q {
        let root = d.root();
        let w = d.find_prefix(root, ns::W).filter(|p| !p.is_empty()).map(str::to_owned);
        let w14 = d.find_prefix(root, ns::W14).filter(|p| !p.is_empty()).map(str::to_owned);
        Q {
            val: w.as_ref().map(|p| format!("{p}:val")),
            id: w.as_ref().map(|p| format!("{p}:id")),
            para_id: w14.as_ref().map(|p| format!("{p}:paraId")),
            text_id: w14.as_ref().map(|p| format!("{p}:textId")),
            w,
        }
    }
}

struct Checker<'a> {
    doc: &'a WordDoc,
    anchors: Option<&'a Anchors>,
    findings: Vec<Finding>,
    // Doc-wide id spaces: id → first holder.
    revision_ids: HashMap<String, (PartIdx, NodeId)>,
    para_ids: HashMap<u32, (PartIdx, NodeId)>,
    docpr_ids: HashMap<String, (PartIdx, NodeId)>,
    // Comments.
    comment_ids: HashMap<String, (PartIdx, NodeId)>,
    comment_para_ids: HashSet<u32>,
    range_starts: HashMap<String, (PartIdx, NodeId)>,
    range_ends: HashMap<String, (PartIdx, NodeId)>,
    references: HashMap<String, (PartIdx, NodeId)>,
    durable_ids: HashSet<u32>,
    ex_parents: Vec<(PartIdx, NodeId, u32)>,
    ex_para_ids: Vec<(PartIdx, NodeId, u32)>,
    extensible_ids: Vec<(PartIdx, NodeId, u32)>,
    // Numbering.
    num_ids: HashSet<String>,
    num_refs: Vec<(PartIdx, NodeId, String)>,
    // Per part, reset in `run_part`.
    bookmark_starts: HashMap<String, NodeId>,
    bookmark_ends: HashMap<String, NodeId>,
    r_prefixes: HashMap<String, bool>,
}

impl<'a> Checker<'a> {
    fn new(doc: &'a WordDoc, anchors: Option<&'a Anchors>) -> Self {
        Checker {
            doc,
            anchors,
            findings: Vec::new(),
            revision_ids: HashMap::new(),
            para_ids: HashMap::new(),
            docpr_ids: HashMap::new(),
            comment_ids: HashMap::new(),
            comment_para_ids: HashSet::new(),
            range_starts: HashMap::new(),
            range_ends: HashMap::new(),
            references: HashMap::new(),
            durable_ids: HashSet::new(),
            ex_parents: Vec::new(),
            ex_para_ids: Vec::new(),
            extensible_ids: Vec::new(),
            num_ids: HashSet::new(),
            num_refs: Vec::new(),
            bookmark_starts: HashMap::new(),
            bookmark_ends: HashMap::new(),
            r_prefixes: HashMap::new(),
        }
    }

    fn report(self) -> Report {
        Report { findings: self.findings }
    }

    fn push_part(&mut self, severity: Severity, code: &str, part: &str, message: String) {
        self.findings.push(Finding {
            code: code.into(),
            part: part.into(),
            path: None,
            anchor: None,
            message,
            severity,
        });
    }

    fn push(&mut self, severity: Severity, code: &str, idx: PartIdx, n: NodeId, message: String) {
        let d = self.doc.part(idx);
        let anchor = self.anchors.and_then(|a| {
            let mut cur = Some(n);
            while let Some(e) = cur {
                if (d.is(e, ns::W, "p") || d.is(e, ns::W, "tbl"))
                    && let Some(h) = a.handle_of(Loc::new(idx, e))
                {
                    return Some(h.to_owned());
                }
                cur = d.parent(e);
            }
            None
        });
        self.findings.push(Finding {
            code: code.into(),
            part: self.doc.part_name(idx).into(),
            path: Some(path_of(d, n)),
            anchor,
            message,
            severity,
        });
    }

    fn run(&mut self) {
        let idxs: Vec<PartIdx> = self.doc.part_indices().collect();
        for idx in idxs {
            self.run_part(idx);
        }
        self.finish();
    }

    fn run_part(&mut self, idx: PartIdx) {
        let doc = self.doc;
        let d = doc.part(idx);
        let kind = doc.part_kind(idx);
        let q = Q::new(d);
        self.bookmark_starts.clear();
        self.bookmark_ends.clear();
        self.r_prefixes.clear();
        let root = d.root();

        if let Some(ign) = d.attr_ns(root, ns::MC, "Ignorable") {
            for p in ign.split_ascii_whitespace() {
                if d.lookup_namespace(root, p).is_none() {
                    self.push(
                        Severity::Error,
                        "xml.ignorable_undeclared",
                        idx,
                        root,
                        format!("mc:Ignorable names prefix {p:?}, which is not declared"),
                    );
                }
            }
        }

        self.element(idx, d, &q, kind, root);
        walk(d, root, |n| {
            if is_ignored_branch(d, n) {
                return false;
            }
            self.element(idx, d, &q, kind, n);
            true
        });

        // Bookmark pairing, per part.
        let mut unpaired: Vec<(NodeId, String, &str)> = Vec::new();
        for (id, &n) in &self.bookmark_starts {
            if !self.bookmark_ends.contains_key(id) {
                unpaired.push((n, id.clone(), "bookmarkStart has no bookmarkEnd"));
            }
        }
        for (id, &n) in &self.bookmark_ends {
            if !self.bookmark_starts.contains_key(id) {
                unpaired.push((n, id.clone(), "bookmarkEnd has no bookmarkStart"));
            }
        }
        unpaired.sort_by_key(|(n, _, _)| n.as_u32());
        for (n, id, msg) in unpaired {
            self.push(Severity::Warning, "bookmark.unpaired", idx, n, format!("{msg} (id {id})"));
        }
    }

    fn wval<'d>(&self, d: &'d Document, q: &Q, n: NodeId, local: &str) -> Option<std::borrow::Cow<'d, str>> {
        let qn = match local {
            "val" => q.val.as_deref(),
            "id" => q.id.as_deref(),
            _ => None,
        };
        match qn {
            Some(qn) => d.attr(n, qn),
            None => d.attr_ns(n, ns::W, local),
        }
    }

    fn element(&mut self, idx: PartIdx, d: &Document, q: &Q, kind: PartKind, n: NodeId) {
        self.check_r_attrs(idx, d, q, n);
        let Some(local) = d.local_name(n) else { return };
        let Some(uri) = d.namespace(n) else { return };
        if uri == ns::W {
            self.w_element(idx, d, q, kind, n, local);
        } else if uri == ns::WP && local == "docPr" {
            if let Some(id) = d.attr(n, "id") {
                let id = id.into_owned();
                if let Some(&(pi, _)) = self.docpr_ids.get(&id) {
                    let other = self.doc.part_name(pi).to_owned();
                    self.push(
                        Severity::Error,
                        "id.docpr_duplicate",
                        idx,
                        n,
                        format!("drawing id {id} is already used (in {other})"),
                    );
                } else {
                    self.docpr_ids.insert(id, (idx, n));
                }
            }
        } else if uri == ns::W15 && local == "commentEx" {
            if let Some(v) = self.hex_attr(idx, d, n, ns::W15, "paraId", true) {
                self.ex_para_ids.push((idx, n, v));
            }
            if let Some(v) = self.hex_attr(idx, d, n, ns::W15, "paraIdParent", false) {
                self.ex_parents.push((idx, n, v));
            }
            if let Some(done) = d.attr_ns(n, ns::W15, "done")
                && !ST_ONOFF.contains(&&*done)
            {
                self.push(Severity::Error, "value.invalid", idx, n, format!("w15:done={done:?} is not on/off"));
            }
        } else if uri == ns::W16CID && local == "commentId" {
            self.hex_attr(idx, d, n, ns::W16CID, "paraId", true);
            if let Some(v) = self.hex_attr(idx, d, n, ns::W16CID, "durableId", true)
                && !self.durable_ids.insert(v)
            {
                self.push(
                    Severity::Error,
                    "id.durable_id_duplicate",
                    idx,
                    n,
                    format!("durableId {v:08X} is used twice"),
                );
            }
        } else if uri == ns::W16CEX
            && local == "commentExtensible"
            && let Some(v) = self.hex_attr(idx, d, n, ns::W16CEX, "durableId", true)
        {
            self.extensible_ids.push((idx, n, v));
        }
    }

    /// Read and check an `ST_LongHexNumber` attribute.
    fn hex_attr(
        &mut self,
        idx: PartIdx,
        d: &Document,
        n: NodeId,
        uri: &str,
        local: &str,
        required: bool,
    ) -> Option<u32> {
        match d.attr_ns(n, uri, local) {
            Some(v) => match long_hex(&v) {
                Some(x) => Some(x),
                None => {
                    self.push(
                        Severity::Error,
                        "id.long_hex_invalid",
                        idx,
                        n,
                        format!("{local}={v:?} is not 8 hex digits in 1..80000000"),
                    );
                    None
                }
            },
            None => {
                if required {
                    self.push(Severity::Error, "id.missing", idx, n, format!("{local} is missing"));
                }
                None
            }
        }
    }

    fn check_r_attrs(&mut self, idx: PartIdx, d: &Document, q: &Q, n: NodeId) {
        let mut bad: Vec<(String, String)> = Vec::new();
        for (k, v) in d.attrs(n) {
            let Some((p, _)) = k.split_once(':') else { continue };
            if p == "xmlns" || p == "xml" || Some(p) == q.w.as_deref() {
                continue;
            }
            let is_r = match self.r_prefixes.get(p) {
                Some(&b) => b,
                None => {
                    let b = d.lookup_namespace(n, p) == Some(ns::R);
                    self.r_prefixes.insert(p.to_owned(), b);
                    b
                }
            };
            if is_r && self.doc.rel(idx, &v).is_none() {
                bad.push((k.to_owned(), v.into_owned()));
            }
        }
        for (k, v) in bad {
            self.push(
                Severity::Error,
                "rel.unresolved",
                idx,
                n,
                format!("{k}={v:?} names no relationship of this part"),
            );
        }
    }

    fn w_element(&mut self, idx: PartIdx, d: &Document, q: &Q, kind: PartKind, n: NodeId, local: &str) {
        match local {
            "p" => {
                self.first_child_rule(idx, d, n, "pPr");
                self.para_id(idx, d, q, kind, n);
            }
            "r" => self.first_child_rule(idx, d, n, "rPr"),
            "tc" => {
                self.first_child_rule(idx, d, n, "tcPr");
                self.cell_ends_with_p(idx, d, n);
            }
            "tr" => {
                self.leading_rule(idx, d, n, &["tblPrEx", "trPr"]);
                self.para_id(idx, d, q, kind, n);
            }
            "tbl" => {
                self.leading_rule(idx, d, n, &["tblPr", "tblGrid"]);
                self.grid_rule(idx, d, q, n);
            }
            "body" => {
                let last = d.child_elements(n).last();
                for c in d.child_elements(n) {
                    if d.is(c, ns::W, "sectPr") && Some(c) != last {
                        self.push(
                            Severity::Error,
                            "order.sectpr_not_last",
                            idx,
                            c,
                            "body-level sectPr must be the last child of w:body".into(),
                        );
                    }
                }
            }
            "pPr" => self.child_order(idx, d, n, "pPr"),
            "rPr" => self.child_order(idx, d, n, "rPr"),
            "tcPr" => self.child_order(idx, d, n, "tcPr"),
            "tblPr" => self.child_order(idx, d, n, "tblPr"),
            "sectPr" => self.child_order(idx, d, n, "sectPr"),
            "trPr" => self.child_order(idx, d, n, "trPr"),
            "settings" if kind == PartKind::Settings => self.child_order(idx, d, n, "settings"),
            "styles" if kind == PartKind::Styles => self.child_order(idx, d, n, "styles"),
            "docDefaults" if kind == PartKind::Styles => self.child_order(idx, d, n, "docDefaults"),
            "style" if kind == PartKind::Styles => self.child_order(idx, d, n, "style"),
            "tblStylePr" if kind == PartKind::Styles => self.child_order(idx, d, n, "tblStylePr"),
            "numbering" => self.numbering_order(idx, d, q, n),
            "jc" => {
                let table = d
                    .parent(n)
                    .is_some_and(|p| d.is(p, ns::W, "tblPr") || d.is(p, ns::W, "trPr") || d.is(p, ns::W, "tblPrEx"));
                let allowed = if table { ST_JC_TABLE } else { ST_JC };
                self.enum_val(idx, d, q, n, allowed);
            }
            "vAlign" => self.enum_val(idx, d, q, n, ST_VALIGN),
            "vMerge" => self.enum_val(idx, d, q, n, &["restart", "continue"]),
            "t" | "delText" | "instrText" | "delInstrText" => self.text_rules(idx, d, n, local),
            "bookmarkStart" | "bookmarkEnd" => {
                if let Some(id) = self.wval(d, q, n, "id") {
                    let id = id.into_owned();
                    if local == "bookmarkStart" {
                        if let Some(id) = insert_new(&mut self.bookmark_starts, id, n) {
                            self.push(
                                Severity::Error,
                                "id.bookmark_duplicate",
                                idx,
                                n,
                                format!("bookmark id {id} starts twice"),
                            );
                        }
                    } else {
                        self.bookmark_ends.entry(id).or_insert(n);
                    }
                }
            }
            "commentRangeStart" | "commentRangeEnd" | "commentReference" => {
                if let Some(id) = self.wval(d, q, n, "id") {
                    let id = id.into_owned();
                    let map = match local {
                        "commentRangeStart" => &mut self.range_starts,
                        "commentRangeEnd" => &mut self.range_ends,
                        _ => &mut self.references,
                    };
                    if let Some(id) = insert_new(map, id, (idx, n))
                        && local != "commentReference"
                    {
                        self.push(
                            Severity::Error,
                            "comment.range_duplicate",
                            idx,
                            n,
                            format!("{local} for comment {id} appears twice"),
                        );
                    }
                }
            }
            "comment" if kind == PartKind::Comments => {
                if let Some(id) = self.wval(d, q, n, "id") {
                    let id = id.into_owned();
                    if let Some(id) = insert_new(&mut self.comment_ids, id, (idx, n)) {
                        self.push(
                            Severity::Error,
                            "id.comment_duplicate",
                            idx,
                            n,
                            format!("comment id {id} is used twice"),
                        );
                    }
                } else {
                    self.push(Severity::Error, "id.missing", idx, n, "comment has no w:id".into());
                }
            }
            "numId" => {
                if d.parent(n).is_some_and(|p| d.is(p, ns::W, "numPr"))
                    && let Some(v) = self.wval(d, q, n, "val")
                    && v != "0"
                {
                    self.num_refs.push((idx, n, v.into_owned()));
                }
            }
            _ => {}
        }
        if REVISIONS.contains(&local) {
            self.revision(idx, d, q, n, local);
        }
        if TOGGLES.contains(&local) && d.parent(n).is_some_and(|p| d.is(p, ns::W, "rPr") || d.is(p, ns::W, "pPr")) {
            self.enum_val(idx, d, q, n, ST_ONOFF);
        }
    }

    fn para_id(&mut self, idx: PartIdx, d: &Document, q: &Q, kind: PartKind, n: NodeId) {
        let pid = match &q.para_id {
            Some(qn) => d.attr(n, qn),
            None => d.attr_ns(n, ns::W14, "paraId"),
        };
        if let Some(v) = pid {
            match long_hex(&v) {
                None => self.push(
                    Severity::Error,
                    "id.para_id_invalid",
                    idx,
                    n,
                    format!("w14:paraId={v:?} is not 8 hex digits in 1..80000000"),
                ),
                Some(x) => {
                    if kind == PartKind::Comments {
                        self.comment_para_ids.insert(x);
                    }
                    if let Some(&(pi, _)) = self.para_ids.get(&x) {
                        let other = self.doc.part_name(pi).to_owned();
                        self.push(
                            Severity::Error,
                            "id.para_id_duplicate",
                            idx,
                            n,
                            format!("w14:paraId {v} is already used (in {other})"),
                        );
                    } else {
                        self.para_ids.insert(x, (idx, n));
                    }
                }
            }
        }
        let tid = match &q.text_id {
            Some(qn) => d.attr(n, qn),
            None => d.attr_ns(n, ns::W14, "textId"),
        };
        if let Some(v) = tid
            && long_hex(&v).is_none()
        {
            self.push(
                Severity::Error,
                "id.text_id_invalid",
                idx,
                n,
                format!("w14:textId={v:?} is not 8 hex digits in 1..80000000"),
            );
        }
    }

    /// `child` (when present) must be the first element child of `n`.
    fn first_child_rule(&mut self, idx: PartIdx, d: &Document, n: NodeId, child: &str) {
        for (i, c) in d.child_elements(n).enumerate() {
            if i > 0 && d.is(c, ns::W, child) {
                let parent = d.local_name(n).unwrap_or("");
                self.push(
                    Severity::Error,
                    "order.properties_not_first",
                    idx,
                    c,
                    format!("w:{child} must be the first child of w:{parent}"),
                );
                return;
            }
        }
    }

    /// The elements in `lead` (when present) must come first, in that order.
    fn leading_rule(&mut self, idx: PartIdx, d: &Document, n: NodeId, lead: &[&str]) {
        let mut last_rank = 0usize;
        let mut in_lead = true;
        for c in d.child_elements(n) {
            let rank = d
                .local_name(c)
                .filter(|_| d.namespace(c) == Some(ns::W))
                .and_then(|l| lead.iter().position(|x| *x == l));
            match rank {
                Some(r) if in_lead && r >= last_rank => last_rank = r,
                Some(_) => {
                    let parent = d.local_name(n).unwrap_or("");
                    let child = d.local_name(c).unwrap_or("");
                    self.push(
                        Severity::Error,
                        "order.properties_not_first",
                        idx,
                        c,
                        format!("w:{child} is out of place in w:{parent} (expected {} first)", lead.join(", ")),
                    );
                    return;
                }
                None => in_lead = false,
            }
        }
    }

    /// Children of a properties element in schema order. Unknown and foreign children are
    /// skipped.
    fn child_order(&mut self, idx: PartIdx, d: &Document, n: NodeId, parent: &'static str) {
        let table = order_table();
        let mut prev: Option<(usize, &str)> = None;
        for c in d.child_elements(n) {
            if d.namespace(c) != Some(ns::W) {
                continue;
            }
            let Some(l) = d.local_name(c) else { continue };
            let Some(&rank) = table.get(&(parent, l)) else { continue };
            if let Some((pr, pl)) = prev
                && rank < pr
            {
                // rPr order is lenient in practice (Word reads any order) and part-level order is
                // not calibrated yet; the rest is strict.
                let sev =
                    if parent == "rPr" || LENIENT.contains(&parent) { Severity::Warning } else { Severity::Error };
                self.push(sev, "order.child_order", idx, c, format!("w:{l} must come before w:{pl} in w:{parent}"));
                return;
            }
            if rank == prev.map_or(usize::MAX, |p| p.0)
                && parent != "trPr"
                && parent != "sectPr"
                && !LENIENT.contains(&parent)
            {
                self.push(
                    Severity::Error,
                    "order.child_duplicate",
                    idx,
                    c,
                    format!("w:{l} appears twice in w:{parent}"),
                );
                return;
            }
            prev = Some((rank, l));
        }
    }

    fn enum_val(&mut self, idx: PartIdx, d: &Document, q: &Q, n: NodeId, allowed: &[&str]) {
        if let Some(v) = self.wval(d, q, n, "val")
            && !allowed.contains(&&*v)
        {
            let l = d.local_name(n).unwrap_or("");
            self.push(
                Severity::Error,
                "value.invalid",
                idx,
                n,
                format!("w:{l} w:val={v:?} is not one of {}", allowed.join("|")),
            );
        }
    }

    fn text_rules(&mut self, idx: PartIdx, d: &Document, n: NodeId, local: &str) {
        // Edge whitespace without xml:space="preserve" is dropped by readers.
        let first = d.first_child(n).and_then(|c| d.text(c));
        let last = d.last_child(n).and_then(|c| d.text(c));
        let edge = first.is_some_and(|t| t.starts_with([' ', '\t'])) || last.is_some_and(|t| t.ends_with([' ', '\t']));
        if edge && d.attr(n, "xml:space").as_deref() != Some("preserve") {
            self.push(
                Severity::Warning,
                "text.space_not_preserved",
                idx,
                n,
                format!("w:{local} has leading or trailing spaces but no xml:space=\"preserve\""),
            );
        }
        if local != "t" && local != "delText" {
            return;
        }
        // Word writes moved-away text as w:t inside w:moveFrom; only w:del requires w:delText.
        let mut in_deletion = false;
        let mut in_move_from = false;
        let mut cur = d.parent(n);
        while let Some(a) = cur {
            if d.namespace(a) == Some(ns::W) {
                match d.local_name(a) {
                    Some("del") => {
                        in_deletion = true;
                        break;
                    }
                    Some("moveFrom") => {
                        in_move_from = true;
                        break;
                    }
                    Some("p" | "body" | "tc" | "txbxContent" | "comment" | "footnote" | "endnote" | "hdr" | "ftr") => {
                        break;
                    }
                    _ => {}
                }
            }
            cur = d.parent(a);
        }
        if local == "t" && in_deletion {
            self.push(
                Severity::Error,
                "track.text_in_deletion",
                idx,
                n,
                "w:t inside a deletion (must be w:delText)".into(),
            );
        } else if local == "delText" && !in_deletion && !in_move_from {
            self.push(
                Severity::Warning,
                "track.deltext_outside_deletion",
                idx,
                n,
                "w:delText outside a deletion is not shown".into(),
            );
        }
    }

    fn revision(&mut self, idx: PartIdx, d: &Document, q: &Q, n: NodeId, local: &str) {
        match self.wval(d, q, n, "id") {
            Some(id) => {
                let id = id.into_owned();
                if let Some(&(pi, _)) = self.revision_ids.get(&id) {
                    let other = self.doc.part_name(pi).to_owned();
                    self.push(
                        Severity::Error,
                        "id.revision_duplicate",
                        idx,
                        n,
                        format!("revision id {id} is already used (in {other})"),
                    );
                } else {
                    self.revision_ids.insert(id, (idx, n));
                }
            }
            None => self.push(Severity::Error, "id.missing", idx, n, format!("w:{local} has no w:id")),
        }
        if !matches!(local, "ins" | "del" | "moveFrom" | "moveTo") {
            return;
        }
        // Markers inside property elements (paragraph mark, row, numbering) carry no content.
        if d.parent(n).and_then(|p| d.local_name(p)).is_some_and(|p| p.ends_with("Pr")) {
            return;
        }
        let mut cur = d.parent(n);
        while let Some(a) = cur {
            if d.namespace(a) == Some(ns::W) {
                let al = d.local_name(a).unwrap_or("");
                if matches!(
                    al,
                    "p" | "body" | "tc" | "txbxContent" | "comment" | "footnote" | "endnote" | "hdr" | "ftr"
                ) {
                    break;
                }
                let bad = al == local
                    || (local == "ins" && matches!(al, "del" | "moveFrom"))
                    || (local == "moveTo" && al == "moveFrom");
                if bad {
                    self.push(Severity::Error, "track.nested", idx, n, format!("w:{local} nested inside w:{al}"));
                    break;
                }
            }
            cur = d.parent(a);
        }
    }

    fn cell_ends_with_p(&mut self, idx: PartIdx, d: &Document, n: NodeId) {
        let last_block = d
            .child_elements(n)
            .filter(|&c| {
                !(d.namespace(c) == Some(ns::W)
                    && d.local_name(c).is_some_and(|l| l == "tcPr" || RANGE_MARKUP.contains(&l)))
            })
            .last();
        match last_block {
            Some(c) if d.is(c, ns::W, "p") => {}
            Some(c) if d.is(c, ns::W, "tbl") => self.push(
                Severity::Error,
                "table.cell_not_ending_in_p",
                idx,
                n,
                "a table cell must end with a paragraph (it ends with a table)".into(),
            ),
            Some(_) => {}
            None => self.push(
                Severity::Error,
                "table.cell_without_p",
                idx,
                n,
                "a table cell must contain a paragraph".into(),
            ),
        }
    }

    fn grid_rule(&mut self, idx: PartIdx, d: &Document, q: &Q, tbl: NodeId) {
        let Some(grid) = d.find_child(tbl, ns::W, "tblGrid") else {
            self.push(Severity::Warning, "table.no_grid", idx, tbl, "table has no w:tblGrid".into());
            return;
        };
        let cols = d.child_elements(grid).filter(|&c| d.is(c, ns::W, "gridCol")).count();
        let int = |c: &Checker<'_>, n: Option<NodeId>| -> usize {
            n.and_then(|n| c.wval(d, q, n, "val")).and_then(|v| v.parse().ok()).unwrap_or(0)
        };
        for tr in d.child_elements(tbl).filter(|&c| d.is(c, ns::W, "tr")) {
            let trpr = d.find_child(tr, ns::W, "trPr");
            let mut sum = int(self, trpr.and_then(|p| d.find_child(p, ns::W, "gridBefore")))
                + int(self, trpr.and_then(|p| d.find_child(p, ns::W, "gridAfter")));
            let mut cells: Vec<NodeId> = Vec::new();
            for c in d.child_elements(tr) {
                if d.is(c, ns::W, "tc") {
                    cells.push(c);
                } else if d.is(c, ns::W, "sdt") || d.is(c, ns::W, "customXml") {
                    let content = d.find_child(c, ns::W, "sdtContent").unwrap_or(c);
                    cells.extend(d.child_elements(content).filter(|&x| d.is(x, ns::W, "tc")));
                }
            }
            for tc in cells {
                let span = d.find_child(tc, ns::W, "tcPr").and_then(|p| d.find_child(p, ns::W, "gridSpan"));
                sum += if span.is_some() { int(self, span).max(1) } else { 1 };
            }
            if sum != cols {
                self.push(
                    Severity::Warning,
                    "table.grid_mismatch",
                    idx,
                    tr,
                    format!("row spans {sum} grid columns, the grid has {cols}"),
                );
                return;
            }
        }
    }

    fn numbering_order(&mut self, idx: PartIdx, d: &Document, q: &Q, root: NodeId) {
        let mut seen_num = false;
        let mut abstract_ids = HashSet::new();
        let mut refs = Vec::new();
        for c in d.child_elements(root) {
            if d.is(c, ns::W, "abstractNum") {
                if seen_num {
                    self.push(Severity::Error, "numbering.order", idx, c, "w:abstractNum after w:num".into());
                    seen_num = false; // report once
                }
                if let Some(id) = d.attr_ns(c, ns::W, "abstractNumId") {
                    abstract_ids.insert(id.into_owned());
                }
            } else if d.is(c, ns::W, "num") {
                seen_num = true;
                if let Some(id) = d.attr_ns(c, ns::W, "numId") {
                    self.num_ids.insert(id.into_owned());
                }
                if let Some(a) = d.find_child(c, ns::W, "abstractNumId").and_then(|a| self.wval(d, q, a, "val")) {
                    refs.push((c, a.into_owned()));
                }
            }
        }
        for (c, a) in refs {
            if !abstract_ids.contains(&a) {
                self.push(
                    Severity::Warning,
                    "numbering.missing_abstract",
                    idx,
                    c,
                    format!("w:num points at abstractNum {a}, which does not exist"),
                );
            }
        }
    }

    /// Cross-part checks once every part is walked.
    fn finish(&mut self) {
        let mut later: Vec<(Severity, &str, PartIdx, NodeId, String)> = Vec::new();
        let has_comments = self.doc.comments_part().is_some();
        for (id, &(pi, n)) in &self.range_starts {
            if !self.range_ends.contains_key(id) {
                later.push((
                    Severity::Error,
                    "comment.range_unclosed",
                    pi,
                    n,
                    format!("commentRangeStart {id} has no commentRangeEnd"),
                ));
            }
            if !self.comment_ids.contains_key(id) {
                later.push((
                    Severity::Error,
                    "comment.missing",
                    pi,
                    n,
                    format!("commentRangeStart {id} names no comment"),
                ));
            }
        }
        for (id, &(pi, n)) in &self.range_ends {
            if !self.range_starts.contains_key(id) {
                later.push((
                    Severity::Error,
                    "comment.range_unopened",
                    pi,
                    n,
                    format!("commentRangeEnd {id} has no commentRangeStart"),
                ));
            }
        }
        for (id, &(pi, n)) in &self.references {
            if !self.comment_ids.contains_key(id) {
                let why = if has_comments { "names no comment" } else { "but there is no comments part" };
                later.push((Severity::Error, "comment.missing", pi, n, format!("commentReference {id} {why}")));
            }
        }
        for (id, &(pi, n)) in &self.comment_ids {
            if !self.references.contains_key(id) {
                later.push((
                    Severity::Warning,
                    "comment.unreferenced",
                    pi,
                    n,
                    format!("comment {id} has no commentReference in the text"),
                ));
            }
        }
        for &(pi, n, v) in &self.ex_para_ids {
            if !self.comment_para_ids.contains(&v) {
                later.push((
                    Severity::Warning,
                    "comment.extension_dangling",
                    pi,
                    n,
                    format!("commentEx paraId {v:08X} is not a comment paragraph"),
                ));
            }
        }
        for &(pi, n, v) in &self.ex_parents {
            if !self.comment_para_ids.contains(&v) {
                later.push((
                    Severity::Error,
                    "comment.parent_dangling",
                    pi,
                    n,
                    format!("reply parent {v:08X} is not a comment paragraph"),
                ));
            }
        }
        for &(pi, n, v) in &self.extensible_ids {
            if !self.durable_ids.is_empty() && !self.durable_ids.contains(&v) {
                later.push((
                    Severity::Warning,
                    "comment.extension_dangling",
                    pi,
                    n,
                    format!("commentExtensible durableId {v:08X} is not in commentsIds"),
                ));
            }
        }
        if self.doc.numbering_part().is_some() {
            for (pi, n, v) in &self.num_refs {
                if !self.num_ids.contains(v) {
                    later.push((
                        Severity::Warning,
                        "numbering.missing_num",
                        *pi,
                        *n,
                        format!("numId {v} does not exist"),
                    ));
                }
            }
        }
        later.sort_by_key(|(_, _, pi, n, _)| (pi.0, n.as_u32()));
        for (sev, code, pi, n, msg) in later {
            self.push(sev, code, pi, n, msg);
        }
    }
}

/// `/w:document[1]/w:body[1]/w:p[3]`: element names with 1-based positions among same-named
/// siblings.
fn path_of(d: &Document, n: NodeId) -> String {
    let mut segs = Vec::new();
    let mut cur = Some(n);
    while let Some(e) = cur {
        if !d.is_element_node(e) {
            break;
        }
        let name = d.name(e).unwrap_or("");
        let pos = match d.parent(e) {
            Some(p) => d.child_elements(p).filter(|&c| d.name(c) == Some(name)).position(|c| c == e).unwrap_or(0) + 1,
            None => 1,
        };
        segs.push(format!("{name}[{pos}]"));
        cur = d.parent(e);
    }
    segs.reverse();
    format!("/{}", segs.join("/"))
}

#[cfg(test)]
mod tests;
