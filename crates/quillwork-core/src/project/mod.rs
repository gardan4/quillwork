//! The read side: the LLM text projection ([`read`], [`text`]), [`find`], the viewer's JSON view
//! model ([`view_model`]), block-level [`patch`]es between two view models and image
//! [`assets`]. Normative description: `spec/projection.md`, `spec/schema/view-model.schema.json`,
//! `spec/schema/patch.schema.json`.
//!
//! Everything here is a pure function of a loaded [`WordDoc`], its [`Anchors`] and its
//! [`StyleSheet`]; nothing is cached between calls and nothing is written back.

mod assets;
mod ctx;
mod find;
mod image;
mod inline;
mod patch;
mod render;
mod select;
pub mod vm;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::anchors::Anchors;
use crate::wml::WordDoc;
use crate::wml::style::StyleSheet;

pub use assets::{Asset, asset_bytes, asset_id, assets};
pub(crate) use assets::{needs_conversion, pixel_size};
pub use find::{DEFAULT_FIND_LIMIT, FindHit, FindRequest, FindResult, MAX_FIND_LIMIT, find};
pub use inline::Marks;
pub use patch::{Patch, Position, Upsert, patch};
pub use render::{ControlField, Fields, Placeholder, RefField};
pub use vm::{ViewModel, ViewModelOptions, view_model};

/// The `spec_version` every JSON shape of this module carries.
pub const SPEC_VERSION: &str = "0.1.0";

/// Default token budget of a [`read`] (o200k_base tokens, estimated).
pub const DEFAULT_BUDGET: usize = 8000;
/// Smallest budget a caller may ask for.
pub const MIN_BUDGET: usize = 500;

/// Which tracked changes the projection shows with markup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum View {
    /// Other authors' changes applied silently; the agent's own open change sets marked up.
    #[default]
    Accepted,
    /// Every tracked change marked up.
    Markup,
}

impl View {
    pub fn as_str(self) -> &'static str {
        match self {
            View::Accepted => "accepted",
            View::Markup => "markup",
        }
    }
}

/// Full text or the headings-only overview.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadMode {
    #[default]
    Full,
    /// Header, outline and every heading with the first sentence after it.
    Outline,
}

/// A change set as the host's session sidecar records it (view-model `ChangeSet`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSetInfo {
    pub id: String,
    pub label: Option<String>,
    pub author: String,
    pub turn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    /// `w:id`s of the tracked revisions in the set.
    pub revisions: Vec<i64>,
    /// Sidecar-only changes Word cannot track (`resize_image img3`, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub untracked: Vec<String>,
    /// `open`, `accepted`, `rejected` or `partial`.
    pub status: String,
}

impl ChangeSetInfo {
    pub fn is_open(&self) -> bool {
        self.status == "open" || self.status == "partial"
    }
}

/// Session facts the document itself doesn't carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectOptions {
    /// The engine's configured agent author (`w:author` of its revisions and comments). It
    /// renders as `AI`.
    pub agent_author: String,
    pub change_sets: Vec<ChangeSetInfo>,
    /// Version to print; defaults to `doc.version() + 1` (the first version is v1).
    pub version: Option<u64>,
    /// File name, the title of last resort.
    pub file_name: Option<String>,
}

impl Default for ProjectOptions {
    fn default() -> Self {
        ProjectOptions { agent_author: "Quillwork".into(), change_sets: Vec::new(), version: None, file_name: None }
    }
}

impl ProjectOptions {
    pub(crate) fn version_of(&self, doc: &WordDoc) -> u64 {
        self.version.unwrap_or(doc.version() + 1)
    }
}

/// A `read_document` call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadRequest {
    pub view: View,
    /// `null` for the first window; else a selection (`b105`, `b105..b10h`, `b10x..`, `..b105`,
    /// `t1.r2`, `h1`, `c1`, comma lists).
    pub range: Option<String>,
    /// Token budget; default [`DEFAULT_BUDGET`], at least [`MIN_BUDGET`].
    pub budget: Option<usize>,
    pub mode: ReadMode,
    /// Also render header/footer/text-box stories with a range read.
    pub stories: bool,
    pub options: ProjectOptions,
}

/// What [`read`] returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadResult {
    pub text: String,
    /// The range for the next window, `None` at the end.
    pub next: Option<String>,
    /// Estimated o200k_base tokens of `text`.
    pub tokens: usize,
}

/// Render the projection (spec/projection.md).
pub fn read(doc: &WordDoc, anchors: &Anchors, sheet: &StyleSheet, req: &ReadRequest) -> Result<ReadResult> {
    render::read(doc, anchors, sheet, None, req)
}

/// [`read`], text only.
pub fn text(doc: &WordDoc, anchors: &Anchors, sheet: &StyleSheet, req: &ReadRequest) -> Result<String> {
    Ok(read(doc, anchors, sheet, req)?.text)
}

/// The projection lines of single blocks (op results: `changed`, `current`), in `view`.
/// Unknown anchors are skipped.
pub fn block_lines(
    doc: &WordDoc,
    anchors: &Anchors,
    sheet: &StyleSheet,
    handles: &[&str],
    view: View,
    options: &ProjectOptions,
) -> Result<Vec<String>> {
    render::block_lines(doc, anchors, sheet, None, handles, view, options)
}

/// The outline line (`§ outline: …`).
pub fn outline_line(doc: &WordDoc, anchors: &Anchors, sheet: &StyleSheet, options: &ProjectOptions) -> Result<String> {
    render::outline_line(doc, anchors, sheet, None, options)
}

/// The style sheet and document-wide lookups of one document version, kept so that many
/// small calls (op results, single-block renders, find, windowed reads) don't each walk the
/// whole document. Build it after every change: a projector used on another version of the
/// document renders wrong anchors, labels and comments.
pub struct Projector {
    sheet: StyleSheet,
    index: ctx::DocIndex,
}

impl Projector {
    pub fn new(doc: &WordDoc, anchors: &Anchors) -> Result<Projector> {
        Ok(Projector { sheet: style_sheet(doc)?, index: ctx::DocIndex::build(doc, anchors) })
    }

    pub fn sheet(&self) -> &StyleSheet {
        &self.sheet
    }

    /// [`read`].
    pub fn read(&self, doc: &WordDoc, anchors: &Anchors, req: &ReadRequest) -> Result<ReadResult> {
        render::read(doc, anchors, &self.sheet, Some(&self.index), req)
    }

    /// [`block_lines`].
    pub fn block_lines(
        &self,
        doc: &WordDoc,
        anchors: &Anchors,
        handles: &[&str],
        view: View,
        options: &ProjectOptions,
    ) -> Result<Vec<String>> {
        render::block_lines(doc, anchors, &self.sheet, Some(&self.index), handles, view, options)
    }

    /// [`outline_line`].
    pub fn outline_line(&self, doc: &WordDoc, anchors: &Anchors, options: &ProjectOptions) -> Result<String> {
        render::outline_line(doc, anchors, &self.sheet, Some(&self.index), options)
    }

    /// Content controls, placeholders and cross-references ([`Fields`]).
    pub fn fields(&self, doc: &WordDoc, anchors: &Anchors, options: &ProjectOptions) -> Result<Fields> {
        render::fields(doc, anchors, &self.sheet, Some(&self.index), options)
    }

    /// [`Projector::fields`] of the given blocks only.
    pub fn fields_for(
        &self,
        doc: &WordDoc,
        anchors: &Anchors,
        options: &ProjectOptions,
        handles: &[&str],
    ) -> Result<Fields> {
        render::fields_for(doc, anchors, &self.sheet, Some(&self.index), options, handles)
    }

    /// View-model blocks of single paragraphs, each built on its own (see `vm::para_blocks`).
    pub(crate) fn para_blocks(
        &self,
        doc: &WordDoc,
        anchors: &Anchors,
        opts: &ViewModelOptions,
        paras: &[crate::wml::Loc],
    ) -> Vec<vm::Block> {
        vm::para_blocks(doc, anchors, &self.sheet, Some(&self.index), opts, paras)
    }

    /// The heading level of each body paragraph outside tables, up to and including `until`.
    pub(crate) fn body_headings(
        &self,
        doc: &WordDoc,
        anchors: &Anchors,
        options: &ProjectOptions,
        until: &str,
    ) -> Vec<(String, Option<u8>)> {
        render::body_headings(doc, anchors, &self.sheet, Some(&self.index), options, until)
    }

    /// Bookmark name → the anchor of the block it sits in.
    pub fn bookmarks(&self) -> &std::collections::HashMap<String, String> {
        &self.index.bookmarks
    }

    /// [`find`].
    pub fn find(&self, doc: &WordDoc, anchors: &Anchors, req: &FindRequest) -> Result<FindResult> {
        find::find_with(doc, anchors, &self.sheet, Some(&self.index), req)
    }

    /// [`view_model`].
    pub fn view_model(&self, doc: &WordDoc, anchors: &Anchors, opts: &ViewModelOptions) -> Result<ViewModel> {
        vm::view_model_with(doc, anchors, &self.sheet, Some(&self.index), opts)
    }
}

/// The document's style sheet, or the built-in defaults when it has no styles part.
pub fn style_sheet(doc: &WordDoc) -> Result<StyleSheet> {
    match doc.style_sheet()? {
        Some(s) => Ok(s),
        None => {
            let d = crate::xml::Document::parse(ctx::MIN_STYLES.as_bytes())?;
            StyleSheet::parse(&d, None)
        }
    }
}

/// Estimated o200k_base token count of projection text: characters / 4.5 plus 5 per line
/// (anchors and short lines tokenize worse than prose). Calibrated with tiktoken on the
/// fixture and private corpora, see `spec/projection.md` §8: it stays within 0.99–1.18× of
/// the real count on real documents and over-estimates synthetic English text by ~1.3×.
pub fn estimate_tokens(s: &str) -> usize {
    let chars = s.chars().count();
    (chars * 10).div_ceil(CHARS_PER_TOKEN_X10) + TOKENS_PER_LINE * s.lines().count()
}

/// Characters per token × 10 used by [`estimate_tokens`].
const CHARS_PER_TOKEN_X10: usize = 45;
/// Extra tokens per line used by [`estimate_tokens`].
const TOKENS_PER_LINE: usize = 5;
