//! Fields (DESIGN §4.1): `w:fldSimple` and complex fields (`w:fldChar` begin / separate / end
//! with `w:instrText` between), which may span runs, hyperlinks and paragraphs and may nest.

use super::text::TextMode;
use super::tree::{Step, is_ignored_branch, is_props, is_w, w_attr, w_local, walk};
use super::view::Run;
use crate::xml::{Document, NodeId};

/// The field type, from the first word of the instruction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FieldKind {
    MergeField,
    Ref,
    PageRef,
    NoteRef,
    Hyperlink,
    Page,
    NumPages,
    SectionPages,
    Date,
    Time,
    CreateDate,
    SaveDate,
    PrintDate,
    Toc,
    Seq,
    If,
    DocProperty,
    DocVariable,
    FormText,
    FormCheckBox,
    FormDropDown,
    Symbol,
    StyleRef,
    Ask,
    Fillin,
    IncludeText,
    IncludePicture,
    Link,
    Dde,
    DdeAuto,
    /// Empty instruction.
    Empty,
    /// Anything else, upper-cased.
    Other(String),
}

impl FieldKind {
    pub fn parse(instr: &str) -> FieldKind {
        let Some(word) = instr.split_whitespace().next() else { return FieldKind::Empty };
        let w = word.to_ascii_uppercase();
        match w.as_str() {
            "MERGEFIELD" => FieldKind::MergeField,
            "REF" => FieldKind::Ref,
            "PAGEREF" => FieldKind::PageRef,
            "NOTEREF" => FieldKind::NoteRef,
            "HYPERLINK" => FieldKind::Hyperlink,
            "PAGE" => FieldKind::Page,
            "NUMPAGES" => FieldKind::NumPages,
            "SECTIONPAGES" => FieldKind::SectionPages,
            "DATE" => FieldKind::Date,
            "TIME" => FieldKind::Time,
            "CREATEDATE" => FieldKind::CreateDate,
            "SAVEDATE" => FieldKind::SaveDate,
            "PRINTDATE" => FieldKind::PrintDate,
            "TOC" => FieldKind::Toc,
            "SEQ" => FieldKind::Seq,
            "IF" => FieldKind::If,
            "DOCPROPERTY" => FieldKind::DocProperty,
            "DOCVARIABLE" => FieldKind::DocVariable,
            "FORMTEXT" => FieldKind::FormText,
            "FORMCHECKBOX" => FieldKind::FormCheckBox,
            "FORMDROPDOWN" => FieldKind::FormDropDown,
            "SYMBOL" => FieldKind::Symbol,
            "STYLEREF" => FieldKind::StyleRef,
            "ASK" => FieldKind::Ask,
            "FILLIN" => FieldKind::Fillin,
            "INCLUDETEXT" => FieldKind::IncludeText,
            "INCLUDEPICTURE" => FieldKind::IncludePicture,
            "LINK" => FieldKind::Link,
            "DDE" => FieldKind::Dde,
            "DDEAUTO" => FieldKind::DdeAuto,
            _ => FieldKind::Other(w),
        }
    }

    /// Kinds whose argument an editor may change (merge field name, bookmark, link target).
    pub fn is_editable(&self) -> bool {
        matches!(self, FieldKind::MergeField | FieldKind::Ref | FieldKind::Hyperlink)
    }

    /// Kinds that pull in or execute external content; never refreshed, flagged to callers.
    pub fn is_active_content(&self) -> bool {
        matches!(
            self,
            FieldKind::Dde | FieldKind::DdeAuto | FieldKind::IncludeText | FieldKind::IncludePicture | FieldKind::Link
        )
    }
}

/// Where a field lives in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldCode {
    /// A `w:fldSimple`; its children are the result.
    Simple { node: NodeId },
    /// A complex field: the `w:fldChar` elements. `separate`/`end` are `None` when missing
    /// (malformed or truncated input).
    Complex { begin: NodeId, separate: Option<NodeId>, end: Option<NodeId> },
}

/// A field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// The instruction, as written (own level only; nested field codes are not inlined).
    pub instr: String,
    pub kind: FieldKind,
    pub code: FieldCode,
    /// Nesting depth (0 = outermost).
    pub depth: usize,
    /// Runs of the displayed result, in document order (nested fields' result runs included).
    pub result_runs: Vec<Run>,
    /// [`FieldKind::is_editable`].
    pub editable: bool,
    /// [`FieldKind::is_active_content`].
    pub active_content: bool,
}

impl Field {
    /// First and last run of the result, `None` when the result is empty.
    pub fn result_range(&self) -> Option<(Run, Run)> {
        Some((*self.result_runs.first()?, *self.result_runs.last()?))
    }

    /// The displayed result in `mode`; runs in different paragraphs are joined with `\n`.
    pub fn result_text(&self, doc: &Document, mode: TextMode) -> String {
        let mut out = String::new();
        let mut last_p: Option<NodeId> = None;
        for r in &self.result_runs {
            let p = doc.ancestors(r.0).find(|&a| is_w(doc, a, "p"));
            if last_p.is_some() && p != last_p {
                out.push('\n');
            }
            last_p = p;
            out.push_str(&r.text(doc, mode));
        }
        out
    }

    /// The first argument of the instruction (`MERGEFIELD Name` → `Name`), quotes stripped.
    pub fn argument(&self) -> Option<String> {
        let rest = self.instr.trim_start();
        let rest = rest[rest.find(char::is_whitespace)?..].trim_start();
        if rest.is_empty() || rest.starts_with('\\') {
            return None;
        }
        if let Some(q) = rest.strip_prefix('"') {
            return Some(q.split('"').next().unwrap_or("").to_owned());
        }
        rest.split_whitespace().next().map(str::to_owned)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Code,
    Result,
}

/// Every field below `root` (a story root) in begin order. Text boxes and ignored `mc`
/// branches are not entered.
pub fn fields(doc: &Document, root: NodeId) -> Vec<Field> {
    let mut out: Vec<Field> = Vec::new();
    // Open complex fields: (index into out, phase).
    let mut open: Vec<(usize, Phase)> = Vec::new();
    walk(doc, root, |n| {
        if is_ignored_branch(doc, n) {
            return Step::Over;
        }
        match w_local(doc, n) {
            Some("txbxContent" | "drawing" | "pict" | "object") => Step::Over,
            Some(l) if is_props(l) => Step::Over,
            Some("fldSimple") => {
                let instr = w_attr(doc, n, "instr").unwrap_or_default();
                let result_runs =
                    doc.descendants(n).filter(|&d| is_w(doc, d, "r") && !in_text_box(doc, d, n)).map(Run).collect();
                let kind = FieldKind::parse(&instr);
                out.push(Field {
                    editable: kind.is_editable(),
                    active_content: kind.is_active_content(),
                    instr,
                    kind,
                    code: FieldCode::Simple { node: n },
                    depth: open.len(),
                    result_runs,
                });
                Step::Into
            }
            Some("r") => {
                let mut special = false;
                for c in doc.child_elements(n) {
                    match w_local(doc, c) {
                        Some("fldChar") => {
                            special = true;
                            match w_attr(doc, c, "fldCharType").as_deref() {
                                Some("begin") => {
                                    out.push(Field {
                                        instr: String::new(),
                                        kind: FieldKind::Empty,
                                        code: FieldCode::Complex { begin: c, separate: None, end: None },
                                        depth: open.len(),
                                        result_runs: Vec::new(),
                                        editable: false,
                                        active_content: false,
                                    });
                                    open.push((out.len() - 1, Phase::Code));
                                }
                                Some("separate") => {
                                    if let Some(top) = open.last_mut() {
                                        top.1 = Phase::Result;
                                        if let FieldCode::Complex { separate, .. } = &mut out[top.0].code {
                                            *separate = Some(c);
                                        }
                                    }
                                }
                                Some("end") => {
                                    if let Some((i, _)) = open.pop()
                                        && let FieldCode::Complex { end, .. } = &mut out[i].code
                                    {
                                        *end = Some(c);
                                    }
                                }
                                _ => {}
                            }
                        }
                        Some("instrText" | "delInstrText") => {
                            special = true;
                            if let Some(&(i, Phase::Code)) = open.last() {
                                out[i].instr.push_str(&doc.text_content(c));
                            }
                        }
                        _ => {}
                    }
                }
                if !special {
                    for &(i, phase) in &open {
                        if phase == Phase::Result {
                            out[i].result_runs.push(Run(n));
                        }
                    }
                }
                Step::Over
            }
            _ => Step::Into,
        }
    });
    for f in &mut out {
        if matches!(f.code, FieldCode::Complex { .. }) {
            f.kind = FieldKind::parse(&f.instr);
            f.editable = f.kind.is_editable();
            f.active_content = f.kind.is_active_content();
        }
    }
    out
}

fn in_text_box(doc: &Document, n: NodeId, stop: NodeId) -> bool {
    doc.ancestors(n).take_while(|&a| a != stop).any(|a| is_w(doc, a, "txbxContent") || is_ignored_branch(doc, a))
}
