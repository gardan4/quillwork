//! The op table: op name → handler. Text, block, format and list ops live in this module's
//! siblings; review, comment and table ops bridge to [`crate::edit`]; `set_track_changes` is
//! here. Catalogue ops without an implementation are `UNSUPPORTED` (`not_implemented`).

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::anchors::Anchors;
use crate::edit::{self, EditOutcome, Mode};
use crate::track::write as tw;
use crate::wml::{PartKind, WordDoc};
use crate::xml::ns;

use super::{Cx, OpError, OpResult, args, list, para, target_key, text};

/// Every op this engine applies (the rest of [`super::OPS`] is `not_implemented`).
pub const IMPLEMENTED: &[&str] = &[
    "replace_text",
    "find_and_replace",
    "insert_paragraph",
    "delete_block",
    "move_block",
    "split_paragraph",
    "merge_paragraphs",
    "set_text_format",
    "set_paragraph",
    "insert_break",
    "set_list",
    "restart_numbering",
    "change_level",
    "insert_table",
    "insert_row",
    "delete_row",
    "set_cell",
    "insert_column",
    "delete_column",
    "merge_cells",
    "split_cell",
    "add_comment",
    "reply_comment",
    "edit_comment",
    "resolve_comment",
    "delete_comment",
    "accept",
    "reject",
    "set_track_changes",
];

/// Ops that may add a package part (numbering, settings, the comment parts). A transaction that
/// holds one snapshots the package so a rollback can drop the new parts.
pub const MAY_ADD_PARTS: &[&str] = &[
    "insert_paragraph",
    "set_list",
    "restart_numbering",
    "change_level",
    "add_comment",
    "reply_comment",
    "edit_comment",
    "resolve_comment",
    "delete_comment",
    "set_track_changes",
];

/// Apply one op (already shape-checked).
pub fn dispatch(cx: &mut Cx, name: &str, op: &Value) -> OpResult<EditOutcome> {
    match name {
        "replace_text" => text::replace_text(cx, op),
        "find_and_replace" => text::find_and_replace(cx, op),
        "split_paragraph" => text::split_paragraph(cx, op),
        "insert_paragraph" => para::insert_paragraph(cx, op),
        "delete_block" => para::delete_block(cx, op),
        "move_block" => para::move_block(cx, op),
        "merge_paragraphs" => para::merge_paragraphs(cx, op),
        "set_paragraph" => para::set_paragraph(cx, op),
        "insert_break" => para::insert_break(cx, op),
        "set_text_format" => super::format::set_text_format(cx, op),
        "set_list" => list::set_list(cx, op),
        "restart_numbering" => list::restart_numbering(cx, op),
        "change_level" => list::change_level(cx, op),
        "insert_table" => bridge(cx, name, op, edit::tables::insert_table, Alloc::Revision),
        "insert_row" => bridge(cx, name, op, edit::tables::insert_row, Alloc::Revision),
        "delete_row" => bridge(cx, name, op, edit::tables::delete_row, Alloc::Revision),
        "set_cell" => bridge(cx, name, op, edit::tables::set_cell, Alloc::Revision),
        "insert_column" => bridge(cx, name, op, edit::tables::insert_column, Alloc::Revision),
        "delete_column" => bridge(cx, name, op, edit::tables::delete_column, Alloc::Revision),
        "merge_cells" => bridge(cx, name, op, edit::tables::merge_cells, Alloc::Revision),
        "split_cell" => bridge(cx, name, op, edit::tables::split_cell, Alloc::Revision),
        "add_comment" => bridge(cx, name, op, edit::comments::add_comment, Alloc::Annotation),
        "reply_comment" => bridge(cx, name, op, edit::comments::reply_comment, Alloc::Annotation),
        "edit_comment" => bridge(cx, name, op, edit::comments::edit_comment, Alloc::Annotation),
        "resolve_comment" => bridge(cx, name, op, edit::comments::resolve_comment, Alloc::Annotation),
        "delete_comment" => bridge(cx, name, op, edit::comments::delete_comment, Alloc::Annotation),
        "accept" => review(cx, op, true),
        "reject" => review(cx, op, false),
        "set_track_changes" => set_track_changes(cx, op),
        other => Err(OpError::unsupported("not_implemented", format!("{other} is not implemented yet"))),
    }
}

// ---------------------------------------------------------------------- edit:: bridge

type EditFn<A> = fn(&mut WordDoc, &mut Anchors, &mut dyn FnMut() -> u32, &str, &str, &A) -> crate::Result<EditOutcome>;

/// Which ids an `edit::*` function's allocator hands out.
#[derive(Clone, Copy)]
enum Alloc {
    /// Tracked-change ids: they join the transaction's change set.
    Revision,
    /// Comment ids: unique in the package, but not revisions.
    Annotation,
}

fn bridge<A: DeserializeOwned>(
    cx: &mut Cx,
    name: &str,
    op: &Value,
    f: EditFn<A>,
    kind: Alloc,
) -> OpResult<EditOutcome> {
    let mut op = op.clone();
    if let Some(o) = op.as_object_mut() {
        let mode = if cx.tk.mode == Mode::Direct { "direct" } else { "track" };
        o.insert("mode".to_owned(), Value::String(mode.to_owned()));
        // The engine checked the guards; the edit functions only read `mode`.
        o.remove("expect");
        o.remove("base_version");
    }
    let a: A = args(cx.index, &op)?;
    let author = cx.tk.stamp.author.clone();
    let date = cx.tk.stamp.date.clone();
    let anchor = target_key(name).and_then(|k| op.get(k)).and_then(Value::as_str).map(str::to_owned);
    let tk = &mut *cx.tk;
    let mut alloc = || match kind {
        Alloc::Revision => tk.rev(),
        Alloc::Annotation => tk.foreign(),
    };
    let r = f(&mut *cx.doc, &mut *cx.anchors, &mut alloc, &author, &date, &a);
    r.map_err(|e| {
        let o = OpError::from(e);
        match &anchor {
            Some(h) if o.code != "UNSUPPORTED" || o.anchor.is_none() => o.with_anchor(h),
            _ => o,
        }
    })
}

// ---------------------------------------------------------------------- review

fn review(cx: &mut Cx, op: &Value, accept: bool) -> OpResult<EditOutcome> {
    let mut a: edit::review::ReviewArgs = args(cx.index, op)?;
    if let Some(cs) = &a.change_set {
        match cx.change_sets.get(cs) {
            Some(revs) => a.change_set_revisions = Some(revs.clone()),
            None => return Err(OpError::not_found("change_set", format!("no change set {cs}"))),
        }
    }
    let author = cx.tk.stamp.author.clone();
    let date = cx.tk.stamp.date.clone();
    let tk = &mut *cx.tk;
    let mut alloc = || tk.foreign();
    let f = if accept { edit::review::accept } else { edit::review::reject };
    f(&mut *cx.doc, &mut *cx.anchors, &mut alloc, &author, &date, &a).map_err(OpError::from)
}

// ---------------------------------------------------------------------- set_track_changes

const SETTINGS_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings";
const SETTINGS_CT: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml";

/// CT_Settings child order (ECMA-376 Part 1, 17.15.1.78). The validator checks against it too.
pub(crate) const SETTINGS_ORDER: &[&str] = &[
    "writeProtection",
    "view",
    "zoom",
    "removePersonalInformation",
    "removeDateAndTime",
    "doNotDisplayPageBoundaries",
    "displayBackgroundShape",
    "printPostScriptOverText",
    "printFractionalCharacterWidth",
    "printFormsData",
    "embedTrueTypeFonts",
    "embedSystemFonts",
    "saveSubsetFonts",
    "saveFormsData",
    "mirrorMargins",
    "alignBordersAndEdges",
    "bordersDoNotSurroundHeader",
    "bordersDoNotSurroundFooter",
    "gutterAtTop",
    "hideSpellingErrors",
    "hideGrammaticalErrors",
    "activeWritingStyle",
    "proofState",
    "formsDesign",
    "attachedTemplate",
    "linkStyles",
    "stylePaneFormatFilter",
    "stylePaneSortMethod",
    "documentType",
    "mailMerge",
    "revisionView",
    "trackRevisions",
    "doNotTrackMoves",
    "doNotTrackFormatting",
    "documentProtection",
    "autoFormatOverride",
    "styleLockTheme",
    "styleLockQFSet",
    "defaultTabStop",
    "autoHyphenation",
    "consecutiveHyphenLimit",
    "hyphenationZone",
    "doNotHyphenateCaps",
    "showEnvelope",
    "summaryLength",
    "clickAndTypeStyle",
    "defaultTableStyle",
    "evenAndOddHeaders",
    "bookFoldRevPrinting",
    "bookFoldPrinting",
    "bookFoldPrintingSheets",
    "drawingGridHorizontalSpacing",
    "drawingGridVerticalSpacing",
    "displayHorizontalDrawingGridEvery",
    "displayVerticalDrawingGridEvery",
    "doNotUseMarginsForDrawingGridOrigin",
    "drawingGridHorizontalOrigin",
    "drawingGridVerticalOrigin",
    "doNotShadeFormData",
    "noPunctuationKerning",
    "characterSpacingControl",
    "printTwoOnOne",
    "strictFirstAndLastChars",
    "noLineBreaksAfter",
    "noLineBreaksBefore",
    "savePreviewPicture",
    "doNotValidateAgainstSchema",
    "saveInvalidXml",
    "ignoreMixedContent",
    "alwaysShowPlaceholderText",
    "doNotDemarcateInvalidXml",
    "saveXmlDataOnly",
    "useXSLTWhenSaving",
    "saveThroughXslt",
    "showXMLTags",
    "alwaysMergeEmptyNamespace",
    "updateFields",
    "hdrShapeDefaults",
    "footnotePr",
    "endnotePr",
    "compat",
    "docVars",
    "rsids",
    "mathPr",
    "attachedSchema",
    "themeFontLang",
    "clrSchemeMapping",
    "doNotIncludeSubdocsInStats",
    "doNotAutoCompressPictures",
    "forceUpgrade",
    "captions",
    "readModeInkLockDown",
    "smartTagType",
    "schemaLibrary",
    "shapeDefaults",
    "doNotEmbedSmartTags",
    "decimalSymbol",
    "listSeparator",
];

#[derive(serde::Deserialize)]
struct TrackArgs {
    on: bool,
}

fn set_track_changes(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: TrackArgs = args(cx.index, op)?;
    let part = match cx.doc.settings_part() {
        Some(p) => p,
        None if !a.on => return Ok(EditOutcome::default()),
        None => {
            let mut name = "word/settings.xml".to_owned();
            let mut i = 1;
            while cx.doc.part_idx(&name).is_some() || cx.doc.package().is_some_and(|p| p.contains(&name)) {
                name = format!("word/settings{i}.xml");
                i += 1;
            }
            let xml = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<w:settings xmlns:w=\"{}\"></w:settings>",
                ns::W
            );
            cx.doc.add_part(&name, PartKind::Settings, SETTINGS_REL, SETTINGS_CT, xml.into_bytes())?
        }
    };
    let current = {
        let d = cx.doc.part(part);
        tw::w_child(d, d.root(), "trackRevisions")
    };
    let is_on = |d: &crate::xml::Document, n| {
        !matches!(tw::w_attr(d, n, "val").as_deref().map(str::trim), Some("false" | "0" | "off"))
    };
    match (current, a.on) {
        (Some(n), true) => {
            if !is_on(cx.doc.part(part), n) {
                tw::remove_w(cx.doc.part_mut(part), n, "val")?;
            }
        }
        (Some(n), false) => cx.doc.part_mut(part).remove(n)?,
        (None, true) => {
            let d = cx.doc.part_mut(part);
            let root = d.root();
            let el = tw::new_w(d, "trackRevisions")?;
            tw::insert_ordered(d, root, el, SETTINGS_ORDER)?;
        }
        (None, false) => {}
    }
    Ok(EditOutcome::default())
}
