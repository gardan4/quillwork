//! `WordDoc`: the parsed XML parts of a Word document, keyed by part name, plus change tracking.

use std::collections::HashSet;
use std::sync::OnceLock;

use super::parts::Parts;
use super::story::{self, Section, Story};
use super::tree::{is_w, parse_hex_id};
use crate::opc::{self, Package, Relationship};
use crate::xml::{self, NodeId, ns};
use crate::{Error, Result};

/// Index of a loaded part in a [`WordDoc`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PartIdx(pub u32);

/// A node in a specific part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Loc {
    pub part: PartIdx,
    pub node: NodeId,
}

impl Loc {
    pub fn new(part: PartIdx, node: NodeId) -> Self {
        Loc { part, node }
    }
}

/// What a loaded part is, from the relationship type that reached it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PartKind {
    Main,
    Header,
    Footer,
    Footnotes,
    Endnotes,
    Comments,
    Numbering,
    Styles,
    Settings,
    Theme,
    /// `word/commentsExtended.xml` (`w15:commentsEx`: reply threading, done state).
    CommentsExtended,
    /// `word/commentsIds.xml` (`w16cid:commentsIds`: durable ids).
    CommentsIds,
    /// `word/commentsExtensible.xml` (`w16cex:commentsExtensible`: UTC dates).
    CommentsExtensible,
    /// `word/people.xml` (`w15:people`: comment authors).
    People,
}

impl PartKind {
    /// From a relationship type URI, by its last path segment (so Strict URIs match too).
    fn from_rel_type(rel_type: &str) -> Option<PartKind> {
        Some(match rel_type.rsplit('/').next().unwrap_or("") {
            "header" => PartKind::Header,
            "footer" => PartKind::Footer,
            "footnotes" => PartKind::Footnotes,
            "endnotes" => PartKind::Endnotes,
            "comments" => PartKind::Comments,
            "numbering" => PartKind::Numbering,
            "styles" => PartKind::Styles,
            "settings" => PartKind::Settings,
            "theme" => PartKind::Theme,
            "commentsExtended" => PartKind::CommentsExtended,
            "commentsIds" => PartKind::CommentsIds,
            "commentsExtensible" => PartKind::CommentsExtensible,
            "people" => PartKind::People,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
struct LoadedPart {
    name: String,
    kind: PartKind,
    doc: xml::Document,
    rels: Vec<Relationship>,
}

/// A Word document: the XML parts the Word model needs, parsed into raw-span trees.
///
/// Loading never changes a byte. Edits go through [`WordDoc::part_mut`]; only parts whose
/// serialization differs from their source are reported by [`WordDoc::modified_parts`].
/// Parts the model does not read (media, glossary, custom XML, …) stay in the package untouched.
#[derive(Debug)]
pub struct WordDoc {
    parts: Vec<LoadedPart>,
    main: PartIdx,
    pkg: Option<Package>,
    /// Parts added by [`WordDoc::add_part`] to a document without a kept package, as
    /// (part name, content type, relationship type); [`WordDoc::write_into`] adds them.
    added: Vec<(String, String, String)>,
    version: u64,
    id_seed: u64,
    id_counter: u64,
    stories: OnceLock<(Vec<Story>, Vec<Section>)>,
}

impl WordDoc {
    /// Load from any [`Parts`] source with default XML limits.
    pub fn load(parts: &dyn Parts) -> Result<WordDoc> {
        Self::load_with(parts, &xml::Limits::default())
    }

    /// Load from any [`Parts`] source.
    pub fn load_with(parts: &dyn Parts, limits: &xml::Limits) -> Result<WordDoc> {
        let pkg_rels = parts.rels("")?;
        let main_name = pkg_rels
            .iter()
            .find(|r| !r.external && r.rel_type.rsplit('/').next() == Some("officeDocument"))
            .and_then(|r| opc::resolve_target("", &r.target))
            .unwrap_or_else(|| "word/document.xml".to_owned());
        let main_doc = parse_part(parts, &main_name, limits)?
            .ok_or_else(|| Error::Wml(format!("main document part {main_name:?} not found")))?;
        let main_rels = parts.rels(&main_name)?;

        let mut loaded =
            vec![LoadedPart { name: main_name.clone(), kind: PartKind::Main, doc: main_doc, rels: main_rels.clone() }];
        let mut seen: HashSet<String> = HashSet::from([main_name.to_lowercase()]);
        for rel in &main_rels {
            if rel.external {
                continue;
            }
            let Some(kind) = PartKind::from_rel_type(&rel.rel_type) else { continue };
            let Some(name) = opc::resolve_target(&main_name, &rel.target) else { continue };
            if !seen.insert(name.to_lowercase()) {
                continue;
            }
            // A dangling relationship is tolerated: the part is simply not there.
            let Some(doc) = parse_part(parts, &name, limits)? else { continue };
            let rels = parts.rels(&name)?;
            loaded.push(LoadedPart { name, kind, doc, rels });
        }
        Ok(WordDoc {
            parts: loaded,
            main: PartIdx(0),
            pkg: None,
            added: Vec::new(),
            version: 0,
            id_seed: 0x5157_494c_4c57_4f52, // "QWILLWOR"
            id_counter: 0,
            stories: OnceLock::new(),
        })
    }

    /// Open a `.docx` from bytes with default package and XML limits. The package is kept, so
    /// [`WordDoc::save`] can write the document back.
    pub fn open(bytes: Vec<u8>) -> Result<WordDoc> {
        Self::open_with(bytes, &opc::Limits::default(), &xml::Limits::default())
    }

    pub fn open_with(bytes: Vec<u8>, pkg_limits: &opc::Limits, xml_limits: &xml::Limits) -> Result<WordDoc> {
        let pkg = Package::open(bytes, pkg_limits)?;
        let mut doc = Self::load_with(&pkg, xml_limits)?;
        doc.pkg = Some(pkg);
        Ok(doc)
    }

    /// The package this document was opened from ([`WordDoc::open`] only).
    pub fn package(&self) -> Option<&Package> {
        self.pkg.as_ref()
    }

    /// Write every modified part into the kept package and return the saved `.docx` bytes.
    /// With no modifications the result is the input, byte for byte.
    pub fn save(&mut self) -> Result<Vec<u8>> {
        let mods = self.modified_parts();
        let pkg = self.pkg.as_mut().ok_or_else(|| Error::Wml("save needs a document opened from a package".into()))?;
        for (name, bytes) in mods {
            pkg.set_part(&name, bytes)?;
        }
        pkg.save()
    }

    /// Write every modified part into `pkg` (for documents loaded from another [`Parts`]).
    pub fn write_into(&self, pkg: &mut Package) -> Result<()> {
        let main = self.part_name(self.main).to_owned();
        for (name, content_type, rel_type) in &self.added {
            if pkg.contains(name) {
                continue;
            }
            let idx = self.part_idx(name).ok_or_else(|| Error::Wml(format!("added part {name:?} is gone")))?;
            pkg.add_part(name, self.part(idx).to_bytes(), content_type)?;
            pkg.add_relationship_to_part(&main, rel_type, name)?;
        }
        for (name, bytes) in self.modified_parts() {
            if !self.added.iter().any(|(n, _, _)| *n == name) {
                pkg.set_part(&name, bytes)?;
            }
        }
        Ok(())
    }

    /// Add a new XML part related from the main document part (comments, comment extensions,
    /// people, …): registers its content type and a relationship of `rel_type`, and loads it as
    /// `kind`. With a kept package the part and relationship are written there right away;
    /// otherwise [`WordDoc::write_into`] adds them.
    pub fn add_part(
        &mut self,
        name: &str,
        kind: PartKind,
        rel_type: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> Result<PartIdx> {
        if self.part_idx(name).is_some() || self.pkg.as_ref().is_some_and(|p| p.contains(name)) {
            return Err(Error::Wml(format!("part {name:?} already exists")));
        }
        let doc = xml::Document::parse(&bytes)?;
        let main = self.part_name(self.main).to_owned();
        let target = opc::relative_target(&main, name);
        let id = match self.pkg.as_mut() {
            Some(pkg) => {
                pkg.add_part(name, bytes, content_type)?;
                pkg.add_relationship_to_part(&main, rel_type, name)?
            }
            None => {
                self.added.push((name.to_owned(), content_type.to_owned(), rel_type.to_owned()));
                let used: HashSet<&str> = self.parts[self.main.0 as usize].rels.iter().map(|r| r.id.as_str()).collect();
                (1u64..).map(|n| format!("rIdQw{n}")).find(|id| !used.contains(id.as_str())).unwrap_or_default()
            }
        };
        self.parts[self.main.0 as usize].rels.push(Relationship {
            id,
            rel_type: rel_type.to_owned(),
            target,
            external: false,
        });
        self.parts.push(LoadedPart { name: name.to_owned(), kind, doc, rels: Vec::new() });
        self.stories = OnceLock::new();
        Ok(PartIdx(self.parts.len() as u32 - 1))
    }

    // ------------------------------------------------------------------ parts

    /// The main document part (`word/document.xml`).
    pub fn main_part(&self) -> PartIdx {
        self.main
    }

    /// All loaded parts.
    pub fn part_indices(&self) -> impl Iterator<Item = PartIdx> + '_ {
        (0..self.parts.len() as u32).map(PartIdx)
    }

    /// A loaded part's tree. Panics on an index not from this document.
    pub fn part(&self, idx: PartIdx) -> &xml::Document {
        &self.parts[idx.0 as usize].doc
    }

    /// Mutable access to a part's tree. Invalidates the cached story list, so story indices
    /// and text-box stories are recomputed on the next [`WordDoc::stories`].
    pub fn part_mut(&mut self, idx: PartIdx) -> &mut xml::Document {
        self.stories = OnceLock::new();
        &mut self.parts[idx.0 as usize].doc
    }

    pub fn part_name(&self, idx: PartIdx) -> &str {
        &self.parts[idx.0 as usize].name
    }

    pub fn part_kind(&self, idx: PartIdx) -> PartKind {
        self.parts[idx.0 as usize].kind
    }

    /// Index of a loaded part by name (case-insensitive, leading `/` ignored).
    pub fn part_idx(&self, name: &str) -> Option<PartIdx> {
        let name = name.strip_prefix('/').unwrap_or(name);
        self.parts.iter().position(|p| p.name.eq_ignore_ascii_case(name)).map(|i| PartIdx(i as u32))
    }

    /// Loaded parts of one kind, in relationship order.
    pub fn parts_of(&self, kind: PartKind) -> impl Iterator<Item = PartIdx> + '_ {
        self.part_indices().filter(move |&i| self.parts[i.0 as usize].kind == kind)
    }

    fn first_of(&self, kind: PartKind) -> Option<PartIdx> {
        self.parts_of(kind).next()
    }

    pub fn styles_part(&self) -> Option<PartIdx> {
        self.first_of(PartKind::Styles)
    }
    pub fn numbering_part(&self) -> Option<PartIdx> {
        self.first_of(PartKind::Numbering)
    }
    pub fn settings_part(&self) -> Option<PartIdx> {
        self.first_of(PartKind::Settings)
    }
    pub fn footnotes_part(&self) -> Option<PartIdx> {
        self.first_of(PartKind::Footnotes)
    }
    pub fn endnotes_part(&self) -> Option<PartIdx> {
        self.first_of(PartKind::Endnotes)
    }
    pub fn comments_part(&self) -> Option<PartIdx> {
        self.first_of(PartKind::Comments)
    }
    pub fn theme_part(&self) -> Option<PartIdx> {
        self.first_of(PartKind::Theme)
    }

    /// Relationships of a loaded part (targets as written).
    pub fn rels(&self, idx: PartIdx) -> &[Relationship] {
        &self.parts[idx.0 as usize].rels
    }

    /// A relationship of `idx` by id (`rId7`).
    pub fn rel(&self, idx: PartIdx, rid: &str) -> Option<&Relationship> {
        self.rels(idx).iter().find(|r| r.id == rid)
    }

    /// The part name an internal relationship of `idx` points at.
    pub fn rel_target_part(&self, idx: PartIdx, rid: &str) -> Option<String> {
        let rel = self.rel(idx, rid)?;
        if rel.external {
            return None;
        }
        opc::resolve_target(self.part_name(idx), &rel.target)
    }

    // ------------------------------------------------------------------ changes

    /// Parts whose serialization differs from their source bytes, as (part name, new bytes).
    pub fn modified_parts(&self) -> Vec<(String, Vec<u8>)> {
        self.parts
            .iter()
            .filter(|p| p.doc.is_modified())
            .filter_map(|p| {
                let bytes = p.doc.to_bytes();
                (bytes != p.doc.source()).then(|| (p.name.clone(), bytes))
            })
            .collect()
    }

    /// The committed-mutation counter. Starts at 0 on load.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Record one committed mutation (a transaction of edits) and return the new version.
    pub fn commit(&mut self) -> u64 {
        self.version += 1;
        self.version
    }

    // ------------------------------------------------------------------ structure

    /// Every story, in a fixed order: body, headers, footers, footnotes, endnotes, comments,
    /// text boxes (see [`story`](super::story)). Cached until the next [`WordDoc::part_mut`].
    pub fn stories(&self) -> &[Story] {
        &self.stories.get_or_init(|| story::discover(self)).0
    }

    /// Sections (`w:sectPr` of the body, in document order).
    pub fn sections(&self) -> &[Section] {
        &self.stories.get_or_init(|| story::discover(self)).1
    }

    /// A [`StyleSheet`](super::style::StyleSheet) over this document's styles, theme,
    /// numbering and settings parts, `None` when there is no styles part.
    pub fn style_sheet(&self) -> Result<Option<super::style::StyleSheet>> {
        let Some(styles) = self.styles_part() else { return Ok(None) };
        let theme = self.theme_part().map(|t| self.part(t));
        let mut sheet = super::style::StyleSheet::parse(self.part(styles), theme)?;
        if let Some(n) = self.numbering_part() {
            sheet.set_numbering(self.part(n))?;
        }
        if let Some(s) = self.settings_part() {
            sheet.set_settings(self.part(s))?;
        }
        Ok(Some(sheet))
    }

    /// The XML tree a story lives in.
    pub fn story_doc(&self, story: &Story) -> &xml::Document {
        self.part(story.part)
    }

    // ------------------------------------------------------------------ paragraph ids

    /// Seed for [`WordDoc::ensure_para_id`]'s id generator. The default seed is fixed, so a
    /// given sequence of edits always yields the same ids.
    pub fn set_id_seed(&mut self, seed: u64) {
        self.id_seed = seed;
        self.id_counter = 0;
    }

    /// The paragraph's `w14:paraId`, stamping a fresh one (plus a `w14:textId`) when it has none.
    /// Ids are unique across all loaded parts, non-zero and below `0x80000000`, written as 8
    /// uppercase hex digits. Declares `w14` on the part's root and lists it in `mc:Ignorable`
    /// when needed. Loading never stamps; only this call does.
    pub fn ensure_para_id(&mut self, loc: Loc) -> Result<u32> {
        let doc = self.part(loc.part);
        if !is_w(doc, loc.node, "p") {
            return Err(Error::Wml("ensure_para_id: not a w:p".into()));
        }
        if let Some(id) = doc.attr_ns(loc.node, ns::W14, "paraId").and_then(|v| parse_hex_id(&v)) {
            return Ok(id);
        }
        let has_text_id = doc.attr_ns(loc.node, ns::W14, "textId").is_some();
        let mut used = self.used_ids();
        let para = self.fresh_id(&used);
        used.insert(para);
        let text = if has_text_id { None } else { Some(self.fresh_id(&used)) };

        let d = self.part_mut(loc.part);
        ensure_w14(d)?;
        d.set_attr_ns(loc.node, ns::W14, "paraId", &format!("{para:08X}"))?;
        if let Some(t) = text {
            d.set_attr_ns(loc.node, ns::W14, "textId", &format!("{t:08X}"))?;
        }
        Ok(para)
    }

    /// Every `w14:paraId` / `w14:textId` in use across the loaded parts.
    fn used_ids(&self) -> HashSet<u32> {
        let mut used = HashSet::new();
        for p in &self.parts {
            let d = &p.doc;
            for n in d.descendants(d.root()) {
                if !d.is_element_node(n) || d.attr_count(n) == 0 {
                    continue;
                }
                for local in ["paraId", "textId"] {
                    if let Some(id) = d.attr_ns(n, ns::W14, local).and_then(|v| parse_hex_id(&v)) {
                        used.insert(id);
                    }
                }
            }
        }
        used
    }

    fn fresh_id(&mut self, used: &HashSet<u32>) -> u32 {
        loop {
            self.id_counter += 1;
            let v =
                (splitmix64(self.id_seed ^ self.id_counter.wrapping_mul(0x9E37_79B9_7F4A_7C15)) & 0x7FFF_FFFF) as u32;
            if v != 0 && !used.contains(&v) {
                return v;
            }
        }
    }
}

fn parse_part(parts: &dyn Parts, name: &str, limits: &xml::Limits) -> Result<Option<xml::Document>> {
    match parts.part(name) {
        Ok(bytes) => Ok(Some(xml::Document::parse_with(bytes.into_owned(), limits)?)),
        Err(Error::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Make `w14` usable on `doc`'s root and ignorable: declare it (and `mc`) when missing and add
/// its prefix to `mc:Ignorable`.
fn ensure_w14(doc: &mut xml::Document) -> Result<()> {
    let root = doc.root();
    let w14 = match doc.find_prefix(root, ns::W14).filter(|p| !p.is_empty()) {
        Some(p) => p.to_owned(),
        None => {
            let p = free_prefix(doc, root, "w14");
            doc.set_attr(root, &format!("xmlns:{p}"), ns::W14)?;
            p
        }
    };
    if doc.find_prefix(root, ns::MC).filter(|p| !p.is_empty()).is_none() {
        let p = free_prefix(doc, root, "mc");
        doc.set_attr(root, &format!("xmlns:{p}"), ns::MC)?;
    }
    let current = doc.attr_ns(root, ns::MC, "Ignorable").map(|c| c.into_owned());
    match current {
        Some(v) if v.split_ascii_whitespace().any(|t| t == w14) => {}
        Some(v) if v.trim().is_empty() => doc.set_attr_ns(root, ns::MC, "Ignorable", &w14)?,
        Some(v) => doc.set_attr_ns(root, ns::MC, "Ignorable", &format!("{v} {w14}"))?,
        None => doc.set_attr_ns(root, ns::MC, "Ignorable", &w14)?,
    }
    Ok(())
}

fn free_prefix(doc: &xml::Document, n: NodeId, want: &str) -> String {
    if doc.lookup_namespace(n, want).is_none() {
        return want.to_owned();
    }
    (1..).map(|i| format!("{want}q{i}")).find(|p| doc.lookup_namespace(n, p).is_none()).expect("unbounded")
}
