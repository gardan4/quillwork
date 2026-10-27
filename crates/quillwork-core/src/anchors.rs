//! Anchors (spec/anchors.md, normative): short stable handles for paragraphs, tables, rows,
//! cells, drawings and comments, plus the sidecar that keeps them stable across sessions.
//!
//! - Body paragraphs are `b` + a Crockford base32 serial (min width 3, first `b100`); story
//!   paragraphs are `<story>-<serial>` (`h1-00`, `fn3-0`; min width 2 for headers/footers, 1
//!   otherwise). Story numbers, `t<n>`, `img<n>` and `c<n>` are document-wide counters from 1 in
//!   document order. Nothing is ever reused.
//! - Rows and cells are positional (`t2.r3.c1`) and resolved on demand.
//! - [`Anchors::mint`] numbers a document the first time; [`Anchors::attach`] re-identifies a
//!   document against a saved [`Sidecar`] (paraId first, then text fingerprint + order);
//!   [`Anchors::sync`] follows in-session edits by node identity.
//! - Minting and attaching never change the document. [`Anchors::ensure_para_id`] is the only
//!   call that writes (a `w14:paraId`), and only when asked.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::wml::text::fingerprint;
use crate::wml::view::{Drawing, Paragraph, Table, drawings, paragraphs, tables};
use crate::wml::{Loc, StoryKind, TextMode, WordDoc};
use crate::{Error, Result};

/// Crockford base32, lowercase.
pub const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Current sidecar format.
pub const SIDECAR_FORMAT: u32 = 1;

// ---------------------------------------------------------------------- grammar

/// Serial `n` in base32, zero-padded to `min_width`.
pub fn encode_serial(mut n: u64, min_width: usize) -> String {
    let mut digits = Vec::new();
    loop {
        digits.push(ALPHABET[(n % 32) as usize]);
        n /= 32;
        if n == 0 {
            break;
        }
    }
    while digits.len() < min_width {
        digits.push(b'0');
    }
    digits.reverse();
    String::from_utf8(digits).expect("ascii")
}

/// `(min width, first serial)` of a story prefix.
pub fn serial_rule(prefix: &str) -> (usize, u64) {
    match prefix {
        "b" => (3, 1024),
        "h" | "f" => (2, 0),
        _ => (1, 0),
    }
}

fn is_b32(c: u8) -> bool {
    ALPHABET.contains(&c)
}

/// `[1-9][0-9]*`
fn is_pos_int(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && (b'1'..=b'9').contains(&b[0]) && b.iter().all(u8::is_ascii_digit)
}

const STORY_PREFIXES: [&str; 6] = ["fn", "en", "tb", "h", "f", "c"];

/// `^(b|(h|f|fn|en|c|tb)[1-9][0-9]*)$`
pub fn is_story_id(s: &str) -> bool {
    s == "b" || STORY_PREFIXES.iter().any(|p| s.strip_prefix(p).is_some_and(is_pos_int))
}

/// `^(b[0-9a-hjkmnp-tv-z]{3,}|(h|f|fn|en|c|tb)[1-9][0-9]*-[0-9a-hjkmnp-tv-z]+)$`
pub fn is_block_anchor(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix('b')
        && rest.len() >= 3
        && rest.bytes().all(is_b32)
    {
        return true;
    }
    let Some((story, serial)) = s.split_once('-') else { return false };
    story != "b" && is_story_id(story) && !serial.is_empty() && serial.bytes().all(is_b32)
}

/// `^t[1-9][0-9]*$`
pub fn is_table_anchor(s: &str) -> bool {
    s.strip_prefix('t').is_some_and(is_pos_int)
}

/// `^img[1-9][0-9]*$`
pub fn is_image_anchor(s: &str) -> bool {
    s.strip_prefix("img").is_some_and(is_pos_int)
}

/// `^c[1-9][0-9]*$`
pub fn is_comment_id(s: &str) -> bool {
    s.strip_prefix('c').is_some_and(is_pos_int)
}

/// `t<n>.r<i>` / `t<n>.r<i>.c<j>` → (table anchor, row, cell).
fn parse_positional(s: &str) -> Option<(&str, usize, Option<usize>)> {
    let mut it = s.split('.');
    let t = it.next()?;
    let r = it.next()?.strip_prefix('r').filter(|r| is_pos_int(r))?;
    let c = match it.next() {
        Some(c) => Some(c.strip_prefix('c').filter(|c| is_pos_int(c))?.parse().ok()?),
        None => None,
    };
    if it.next().is_some() || !is_table_anchor(t) {
        return None;
    }
    Some((t, r.parse().ok()?, c))
}

/// `^t[1-9][0-9]*\.r[1-9][0-9]*$`
pub fn is_row_anchor(s: &str) -> bool {
    parse_positional(s).is_some_and(|(_, _, c)| c.is_none())
}

/// `^t[1-9][0-9]*\.r[1-9][0-9]*\.c[1-9][0-9]*$`
pub fn is_cell_anchor(s: &str) -> bool {
    parse_positional(s).is_some_and(|(_, _, c)| c.is_some())
}

/// Any anchor kind.
pub fn is_anchor(s: &str) -> bool {
    is_block_anchor(s)
        || is_table_anchor(s)
        || is_row_anchor(s)
        || is_cell_anchor(s)
        || is_image_anchor(s)
        || is_comment_id(s)
}

// ---------------------------------------------------------------------- sidecar

/// The persisted anchor map, versioned with the document (spec/anchors.md, "Persistence").
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Sidecar {
    pub format: u32,
    /// Stories with their identity and serial counter, in document order.
    pub stories: Vec<SidecarStory>,
    /// Next story number per prefix (`h`, `f`, `fn`, `en`, `c`, `tb`).
    pub next_story: BTreeMap<String, u32>,
    pub next_table: u32,
    pub next_image: u32,
    /// Paragraph handle → identity.
    pub blocks: BTreeMap<String, BlockEntry>,
    /// `t<n>` → identity.
    pub tables: BTreeMap<String, TableEntry>,
    /// `img<n>` → identity.
    pub images: BTreeMap<String, ImageEntry>,
    /// `c<n>` → `w:comment/@w:id`.
    pub comments: BTreeMap<String, CommentEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarStory {
    /// Story id (`b`, `h1`, `fn3`, …).
    pub id: String,
    /// What the story is in the package: `body`, `part:<name>`, `fn:<w:id>`, `en:<w:id>`,
    /// `c:<w:id>`, `tb:<host story>:d<docPr id>` / `tb:<host story>:o<ordinal>`.
    pub ident: String,
    /// Next serial to mint in this story.
    pub next_serial: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockEntry {
    pub story: String,
    /// `w14:paraId` (8 hex digits) when the paragraph has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub para_id: Option<String>,
    /// 64-bit FNV-1a of the as-is text, 16 hex digits.
    pub fingerprint: String,
    /// Index among the story's paragraphs.
    pub position: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableEntry {
    pub story: String,
    /// Handle of the first cell paragraph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_block: Option<String>,
    /// paraId of the first cell paragraph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub para_id: Option<String>,
    /// Index among the story's tables.
    pub position: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageEntry {
    pub story: String,
    /// `wp:docPr/@id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_pr_id: Option<u32>,
    /// Handle of the paragraph holding the drawing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
    /// paraId of that paragraph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub para_id: Option<String>,
    /// Index among the story's drawings.
    pub position: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentEntry {
    pub comment_id: i64,
}

impl Sidecar {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("sidecar serializes")
    }
    pub fn from_json(s: &str) -> Result<Sidecar> {
        let sc: Sidecar = serde_json::from_str(s).map_err(|e| Error::Anchor(format!("sidecar: {e}")))?;
        if sc.format != SIDECAR_FORMAT {
            return Err(Error::Anchor(format!("sidecar: unsupported format {}", sc.format)));
        }
        Ok(sc)
    }
}

// ---------------------------------------------------------------------- live map

/// What an anchor points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A paragraph; `story` indexes [`WordDoc::stories`].
    Block {
        story: usize,
        para: Loc,
    },
    Table {
        table: Loc,
    },
    Row {
        table: Loc,
        row: Loc,
    },
    Cell {
        table: Loc,
        row: Loc,
        cell: Loc,
    },
    Image {
        drawing: Loc,
    },
    /// A comment (`w:comment`); `story` is its story.
    Comment {
        story: usize,
        comment: Loc,
    },
}

#[derive(Debug, Clone)]
struct StoryState {
    id: String,
    ident: String,
    next_serial: u64,
    /// Paragraph handles in document order.
    blocks: Vec<(String, Loc)>,
}

/// The live anchor map of one loaded document.
#[derive(Debug, Clone, Default)]
pub struct Anchors {
    stories: Vec<StoryState>,
    next_story: BTreeMap<String, u32>,
    next_table: u32,
    next_image: u32,
    /// `t<n>` in document order, with its story index.
    tables: Vec<(String, usize, Loc)>,
    /// `img<n>` in document order, with its story index.
    images: Vec<(String, usize, Loc)>,
    by_handle: HashMap<String, Slot>,
    by_loc: HashMap<Loc, String>,
}

#[derive(Debug, Clone, Copy)]
enum Slot {
    Block(usize, Loc),
    Table(Loc),
    Image(Loc),
}

/// Where the previous identities come from.
enum Prev<'a> {
    None,
    Sidecar(&'a Sidecar),
    Session(&'a Anchors),
}

impl Anchors {
    /// Number a document for the first time.
    pub fn mint(doc: &WordDoc) -> Anchors {
        build(doc, Prev::None)
    }

    /// Re-identify a document against a sidecar saved with an earlier version of it.
    /// Paragraphs, tables and drawings that can't be matched get new handles.
    pub fn attach(doc: &WordDoc, sidecar: &Sidecar) -> Result<Anchors> {
        if sidecar.format != SIDECAR_FORMAT {
            return Err(Error::Anchor(format!("unsupported sidecar format {}", sidecar.format)));
        }
        Ok(build(doc, Prev::Sidecar(sidecar)))
    }

    /// Follow edits made to `doc` in this session: nodes still in the tree keep their handles,
    /// new ones get fresh serials, removed ones' handles stop resolving.
    pub fn sync(&mut self, doc: &WordDoc) {
        *self = build(doc, Prev::Session(self));
    }

    /// The sidecar for the document as it is now (call [`Anchors::sync`] after edits first).
    pub fn sidecar(&self, doc: &WordDoc) -> Sidecar {
        let mut sc = Sidecar {
            format: SIDECAR_FORMAT,
            next_story: self.next_story.clone(),
            next_table: self.next_table,
            next_image: self.next_image,
            ..Default::default()
        };
        let stories = doc.stories();
        for (si, s) in self.stories.iter().enumerate() {
            sc.stories.push(SidecarStory { id: s.id.clone(), ident: s.ident.clone(), next_serial: s.next_serial });
            for (pos, (h, loc)) in s.blocks.iter().enumerate() {
                let d = doc.part(loc.part);
                let p = Paragraph(loc.node);
                sc.blocks.insert(
                    h.clone(),
                    BlockEntry {
                        story: s.id.clone(),
                        para_id: p.para_id(d).map(hex8),
                        fingerprint: format!("{:016x}", fingerprint(&p.text(d, TextMode::AsIs))),
                        position: pos as u32,
                    },
                );
            }
            if let Some(StoryKind::Comment { id }) = stories.get(si).map(|st| &st.kind) {
                sc.comments.insert(s.id.clone(), CommentEntry { comment_id: *id });
            }
        }
        let mut pos_in_story: HashMap<usize, u32> = HashMap::new();
        for (t, si, loc) in &self.tables {
            let d = doc.part(loc.part);
            let first = Table(loc.node).first_paragraph(d);
            let pos = pos_in_story.entry(*si).or_default();
            sc.tables.insert(
                t.clone(),
                TableEntry {
                    story: self.stories[*si].id.clone(),
                    first_block: first.and_then(|p| self.by_loc.get(&Loc::new(loc.part, p.0)).cloned()),
                    para_id: first.and_then(|p| p.para_id(d)).map(hex8),
                    position: *pos,
                },
            );
            *pos += 1;
        }
        let mut pos_in_story: HashMap<usize, u32> = HashMap::new();
        for (img, si, loc) in &self.images {
            let d = doc.part(loc.part);
            let dr = Drawing(loc.node);
            let para = dr.paragraph(d);
            let pos = pos_in_story.entry(*si).or_default();
            sc.images.insert(
                img.clone(),
                ImageEntry {
                    story: self.stories[*si].id.clone(),
                    doc_pr_id: dr.doc_pr_id(d),
                    block: para.and_then(|p| self.by_loc.get(&Loc::new(loc.part, p.0)).cloned()),
                    para_id: para.and_then(|p| p.para_id(d)).map(hex8),
                    position: *pos,
                },
            );
            *pos += 1;
        }
        sc
    }

    /// Story id of `doc.stories()[i]` (`b`, `h1`, `fn3`, …).
    pub fn story_id(&self, i: usize) -> Option<&str> {
        self.stories.get(i).map(|s| s.id.as_str())
    }

    /// Index into `doc.stories()` of a story id.
    pub fn story_index(&self, id: &str) -> Option<usize> {
        self.stories.iter().position(|s| s.id == id)
    }

    /// Paragraph handles of a story in document order.
    pub fn handles(&self, story: usize) -> impl Iterator<Item = (&str, Loc)> + '_ {
        self.stories.get(story).into_iter().flat_map(|s| s.blocks.iter().map(|(h, l)| (h.as_str(), *l)))
    }

    /// Every table anchor in document order.
    pub fn table_anchors(&self) -> impl Iterator<Item = (&str, Loc)> + '_ {
        self.tables.iter().map(|(t, _, l)| (t.as_str(), *l))
    }

    /// Every image anchor in document order.
    pub fn image_anchors(&self) -> impl Iterator<Item = (&str, Loc)> + '_ {
        self.images.iter().map(|(t, _, l)| (t.as_str(), *l))
    }

    /// The handle of a paragraph (`w:p`), table (`w:tbl`) or drawing.
    pub fn handle_of(&self, loc: Loc) -> Option<&str> {
        self.by_loc.get(&loc).map(String::as_str)
    }

    /// Resolve any anchor. Rows and cells are positional in the current tree.
    pub fn resolve(&self, doc: &WordDoc, handle: &str) -> Option<Target> {
        if let Some(slot) = self.by_handle.get(handle) {
            return Some(match *slot {
                Slot::Block(story, para) => Target::Block { story, para },
                Slot::Table(table) => Target::Table { table },
                Slot::Image(drawing) => Target::Image { drawing },
            });
        }
        if is_comment_id(handle) {
            let story = self.story_index(handle)?;
            let st = doc.stories().get(story)?;
            return Some(Target::Comment { story, comment: Loc::new(st.part, st.root) });
        }
        let (t, r, c) = parse_positional(handle)?;
        let Slot::Table(table) = *self.by_handle.get(t)? else { return None };
        let d = doc.part(table.part);
        let row = *Table(table.node).rows(d).get(r - 1)?;
        let row_loc = Loc::new(table.part, row.0);
        match c {
            None => Some(Target::Row { table, row: row_loc }),
            Some(c) => {
                let cell = *row.cells(d).get(c - 1)?;
                Some(Target::Cell { table, row: row_loc, cell: Loc::new(table.part, cell.0) })
            }
        }
    }

    /// Stamp a `w14:paraId` on the paragraph behind `handle` if it has none (so the handle
    /// survives outside edits better) and return it.
    pub fn ensure_para_id(&mut self, doc: &mut WordDoc, handle: &str) -> Result<u32> {
        match self.by_handle.get(handle) {
            Some(Slot::Block(_, loc)) => doc.ensure_para_id(*loc),
            _ => Err(Error::Anchor(format!("not a paragraph handle: {handle}"))),
        }
    }
}

fn hex8(id: u32) -> String {
    format!("{id:08X}")
}

fn parse_fp(s: &str) -> Option<u64> {
    u64::from_str_radix(s, 16).ok()
}

// ---------------------------------------------------------------------- building

fn build(doc: &WordDoc, prev: Prev<'_>) -> Anchors {
    let stories = doc.stories();
    let mut out = Anchors {
        next_story: match &prev {
            Prev::None => BTreeMap::new(),
            Prev::Sidecar(sc) => sc.next_story.clone(),
            Prev::Session(a) => a.next_story.clone(),
        },
        next_table: match &prev {
            Prev::None => 1,
            Prev::Sidecar(sc) => sc.next_table.max(1),
            Prev::Session(a) => a.next_table,
        },
        next_image: match &prev {
            Prev::None => 1,
            Prev::Sidecar(sc) => sc.next_image.max(1),
            Prev::Session(a) => a.next_image,
        },
        ..Default::default()
    };

    // Old stories by ident.
    let old_stories: Vec<(String, String, u64)> = match &prev {
        Prev::None => Vec::new(),
        Prev::Sidecar(sc) => sc.stories.iter().map(|s| (s.id.clone(), s.ident.clone(), s.next_serial)).collect(),
        Prev::Session(a) => a.stories.iter().map(|s| (s.id.clone(), s.ident.clone(), s.next_serial)).collect(),
    };
    let old_by_ident: HashMap<&str, usize> = old_stories.iter().enumerate().map(|(i, s)| (s.1.as_str(), i)).collect();
    // Old story numbers must never be handed out again, also when the sidecar's counters lag.
    for (id, _, _) in &old_stories {
        bump_counter(&mut out.next_story, id);
    }

    // Sidecar blocks grouped by story id, in position order.
    let mut old_blocks: HashMap<&str, Vec<(&str, &BlockEntry)>> = HashMap::new();
    if let Prev::Sidecar(sc) = &prev {
        for (h, e) in &sc.blocks {
            old_blocks.entry(e.story.as_str()).or_default().push((h.as_str(), e));
        }
        for v in old_blocks.values_mut() {
            v.sort_by_key(|(h, e)| (e.position, *h));
        }
    }

    // Stories and their paragraphs.
    let mut tb_ordinals: HashMap<usize, u32> = HashMap::new();
    let mut ident_seen: HashMap<String, u32> = HashMap::new();
    for (si, story) in stories.iter().enumerate() {
        let prefix = story.prefix();
        let mut ident = match &story.kind {
            StoryKind::Body => "body".to_owned(),
            StoryKind::Header { .. } | StoryKind::Footer { .. } => format!("part:{}", doc.part_name(story.part)),
            StoryKind::Footnote { id } => format!("fn:{id}"),
            StoryKind::Endnote { id } => format!("en:{id}"),
            StoryKind::Comment { id } => format!("c:{id}"),
            StoryKind::TextBox { host, drawing_id, .. } => {
                let host_id = out.stories.get(*host).map_or("?", |s| s.id.as_str());
                match drawing_id {
                    Some(d) => format!("tb:{host_id}:d{d}"),
                    None => {
                        let k = tb_ordinals.entry(*host).or_default();
                        *k += 1;
                        format!("tb:{host_id}:o{k}")
                    }
                }
            }
        };
        let seen = ident_seen.entry(ident.clone()).or_default();
        *seen += 1;
        if *seen > 1 {
            ident = format!("{ident}#{seen}");
        }

        let (id, mut next_serial) = match old_by_ident.get(ident.as_str()) {
            Some(&oi) if old_stories[oi].0.starts_with(prefix) && !id_in_use(&out, &old_stories[oi].0) => {
                (old_stories[oi].0.clone(), old_stories[oi].2)
            }
            _ => {
                let id = if prefix == "b" {
                    "b".to_owned()
                } else {
                    let n = out.next_story.entry(prefix.to_owned()).or_insert(1);
                    let id = format!("{prefix}{n}");
                    *n += 1;
                    id
                };
                (id, serial_rule(prefix).1)
            }
        };
        let (width, first) = serial_rule(prefix);
        next_serial = next_serial.max(first);

        let d = doc.part(story.part);
        let paras = paragraphs(d, story.root);
        let mut handles: Vec<Option<String>> = vec![None; paras.len()];
        match &prev {
            Prev::None => {}
            Prev::Session(a) => {
                if let Some(&oi) = old_by_ident.get(ident.as_str()) {
                    let old: HashMap<Loc, &str> = a.stories[oi].blocks.iter().map(|(h, l)| (*l, h.as_str())).collect();
                    for (i, p) in paras.iter().enumerate() {
                        if let Some(h) = old.get(&Loc::new(story.part, p.0)) {
                            handles[i] = Some((*h).to_owned());
                        }
                    }
                }
            }
            Prev::Sidecar(_) => {
                if let Some(old) = old_blocks.get(id.as_str()).filter(|_| old_by_ident.contains_key(ident.as_str())) {
                    match_paragraphs(d, &paras, old, &mut handles);
                }
            }
        }
        let mut blocks = Vec::with_capacity(paras.len());
        for (p, h) in paras.iter().zip(handles) {
            let h = h.unwrap_or_else(|| {
                let s = encode_serial(next_serial, width);
                next_serial += 1;
                if prefix == "b" { format!("b{s}") } else { format!("{id}-{s}") }
            });
            let loc = Loc::new(story.part, p.0);
            out.by_handle.insert(h.clone(), Slot::Block(si, loc));
            out.by_loc.insert(loc, h.clone());
            blocks.push((h, loc));
        }
        out.stories.push(StoryState { id, ident, next_serial, blocks });
    }

    // Tables.
    let old_tables: Vec<(&str, &TableEntry)> = match &prev {
        Prev::Sidecar(sc) => sc.tables.iter().map(|(t, e)| (t.as_str(), e)).collect(),
        _ => Vec::new(),
    };
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let by_first: HashMap<&str, &str> =
        old_tables.iter().filter_map(|(t, e)| Some((e.first_block.as_deref()?, *t))).collect();
    let session_tables: HashMap<Loc, &str> = match &prev {
        Prev::Session(a) => a.tables.iter().map(|(t, _, l)| (*l, t.as_str())).collect(),
        _ => HashMap::new(),
    };
    for (t, _) in &old_tables {
        bump_numbered(&mut out.next_table, t, "t");
    }
    for (si, story) in stories.iter().enumerate() {
        let d = doc.part(story.part);
        for t in tables(d, story.root) {
            let loc = Loc::new(story.part, t.0);
            let first = t.first_paragraph(d).and_then(|p| out.by_loc.get(&Loc::new(story.part, p.0)).cloned());
            let found = session_tables
                .get(&loc)
                .map(|s| (*s).to_owned())
                .or_else(|| first.as_deref().and_then(|f| by_first.get(f)).map(|s| (*s).to_owned()))
                .filter(|t| !used.contains(t));
            let name = found.unwrap_or_else(|| {
                let n = format!("t{}", out.next_table);
                out.next_table += 1;
                n
            });
            used.insert(name.clone());
            out.by_handle.insert(name.clone(), Slot::Table(loc));
            out.by_loc.insert(loc, name.clone());
            out.tables.push((name, si, loc));
        }
    }

    // Drawings.
    let old_images: Vec<(&str, &ImageEntry)> = match &prev {
        Prev::Sidecar(sc) => sc.images.iter().map(|(t, e)| (t.as_str(), e)).collect(),
        _ => Vec::new(),
    };
    for (t, _) in &old_images {
        bump_numbered(&mut out.next_image, t, "img");
    }
    let session_images: HashMap<Loc, &str> = match &prev {
        Prev::Session(a) => a.images.iter().map(|(t, _, l)| (*l, t.as_str())).collect(),
        _ => HashMap::new(),
    };
    let mut img_used: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut cur: Vec<(usize, Loc, Option<String>, Option<u32>)> = Vec::new();
    for (si, story) in stories.iter().enumerate() {
        let d = doc.part(story.part);
        for dr in drawings(d, story.root) {
            let block = dr.paragraph(d).and_then(|p| out.by_loc.get(&Loc::new(story.part, p.0)).cloned());
            cur.push((si, Loc::new(story.part, dr.0), block, dr.doc_pr_id(d)));
        }
    }
    let mut names: Vec<Option<String>> = vec![None; cur.len()];
    // Pass 1: session identity, then (block, docPr id).
    for (i, (_, loc, block, dpr)) in cur.iter().enumerate() {
        let found = session_images.get(loc).copied().or_else(|| {
            old_images
                .iter()
                .find(|(n, e)| !img_used.contains(n) && block.is_some() && e.block == *block && e.doc_pr_id == *dpr)
                .map(|(n, _)| *n)
        });
        if let Some(n) = found.filter(|n| !img_used.contains(n)) {
            img_used.insert(n);
            names[i] = Some(n.to_owned());
        }
    }
    // Pass 2: same paragraph, in order.
    for (i, (_, _, block, _)) in cur.iter().enumerate() {
        if names[i].is_some() || block.is_none() {
            continue;
        }
        if let Some((n, _)) = old_images.iter().find(|(n, e)| !img_used.contains(n) && e.block == *block) {
            img_used.insert(n);
            names[i] = Some((*n).to_owned());
        }
    }
    for ((si, loc, _, _), name) in cur.into_iter().zip(names) {
        let name = name.unwrap_or_else(|| {
            let n = format!("img{}", out.next_image);
            out.next_image += 1;
            n
        });
        out.by_handle.insert(name.clone(), Slot::Image(loc));
        out.by_loc.insert(loc, name.clone());
        out.images.push((name, si, loc));
    }
    out
}

fn id_in_use(a: &Anchors, id: &str) -> bool {
    a.stories.iter().any(|s| s.id == id)
}

/// Raise the story counter for `id`'s prefix past `id`'s number.
fn bump_counter(next: &mut BTreeMap<String, u32>, id: &str) {
    for p in STORY_PREFIXES {
        if let Some(n) = id.strip_prefix(p).filter(|n| is_pos_int(n)).and_then(|n| n.parse::<u32>().ok()) {
            let e = next.entry(p.to_owned()).or_insert(1);
            *e = (*e).max(n + 1);
            return;
        }
    }
}

fn bump_numbered(next: &mut u32, name: &str, prefix: &str) {
    if let Some(n) = name.strip_prefix(prefix).and_then(|n| n.parse::<u32>().ok()) {
        *next = (*next).max(n + 1);
    }
}

/// Match a story's current paragraphs against its sidecar entries: unique paraIds first, then
/// the remaining ones by text fingerprint in order.
fn match_paragraphs(
    d: &crate::xml::Document,
    paras: &[Paragraph],
    old: &[(&str, &BlockEntry)],
    handles: &mut [Option<String>],
) {
    let new_ids: Vec<Option<String>> = paras.iter().map(|p| p.para_id(d).map(hex8)).collect();
    let mut count_new: HashMap<&str, u32> = HashMap::new();
    for id in new_ids.iter().flatten() {
        *count_new.entry(id).or_default() += 1;
    }
    let mut old_by_id: HashMap<&str, (usize, u32)> = HashMap::new();
    for (oi, (_, e)) in old.iter().enumerate() {
        if let Some(id) = e.para_id.as_deref() {
            let ent = old_by_id.entry(id).or_insert((oi, 0));
            ent.1 += 1;
        }
    }
    let mut old_taken = vec![false; old.len()];
    for (i, id) in new_ids.iter().enumerate() {
        let Some(id) = id.as_deref() else { continue };
        if count_new.get(id) != Some(&1) {
            continue;
        }
        if let Some(&(oi, 1)) = old_by_id.get(id) {
            old_taken[oi] = true;
            handles[i] = Some(old[oi].0.to_owned());
        }
    }

    // Fingerprint diff over what is left, in order.
    let rest_new: Vec<usize> = (0..paras.len()).filter(|&i| handles[i].is_none()).collect();
    let rest_old: Vec<usize> = (0..old.len()).filter(|&i| !old_taken[i]).collect();
    if rest_new.is_empty() || rest_old.is_empty() {
        return;
    }
    let new_fp: Vec<u64> = rest_new.iter().map(|&i| fingerprint(&paras[i].text(d, TextMode::AsIs))).collect();
    let old_fp: Vec<u64> = rest_old.iter().map(|&i| parse_fp(&old[i].1.fingerprint).unwrap_or(0)).collect();
    // A paragraph that kept its paraId but lost the match above (duplicated id) must not be
    // paired with an old entry that had a different paraId.
    let new_pid: Vec<Option<&str>> = rest_new.iter().map(|&i| new_ids[i].as_deref()).collect();
    let old_pid: Vec<Option<&str>> = rest_old.iter().map(|&i| old[i].1.para_id.as_deref()).collect();
    let conflict = |ni: usize, oi: usize| matches!((old_pid[oi], new_pid[ni]), (Some(a), Some(b)) if a != b);
    let mut old_used = vec![false; rest_old.len()];
    for (ni, oi) in diff(&old_fp, &new_fp) {
        if conflict(ni, oi) {
            continue;
        }
        old_used[oi] = true;
        handles[rest_new[ni]] = Some(old[rest_old[oi]].0.to_owned());
    }

    // Moves: a paragraph the ordered diff left out whose text is unique among the leftovers on
    // both sides keeps its handle.
    let mut left: HashMap<u64, (u32, usize, u32, usize)> = HashMap::new();
    for (oi, &fp) in old_fp.iter().enumerate().filter(|&(oi, _)| !old_used[oi]) {
        let e = left.entry(fp).or_insert((0, oi, 0, 0));
        e.0 += 1;
    }
    for (ni, &fp) in new_fp.iter().enumerate().filter(|&(ni, _)| handles[rest_new[ni]].is_none()) {
        if let Some(e) = left.get_mut(&fp) {
            e.2 += 1;
            e.3 = ni;
        }
    }
    for &(co, oi, cn, ni) in left.values() {
        if co == 1 && cn == 1 && !conflict(ni, oi) {
            handles[rest_new[ni]] = Some(old[rest_old[oi]].0.to_owned());
        }
    }
}

/// Pair up two sequences of fingerprints: common prefix/suffix, then unique-element anchors
/// (patience), recursing into the gaps; equal-length gaps pair positionally, small gaps by LCS.
/// Returns `(new index, old index)` pairs.
fn diff(old: &[u64], new: &[u64]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut work = vec![(0usize, old.len(), 0usize, new.len())];
    while let Some((mut o0, mut o1, mut n0, mut n1)) = work.pop() {
        while o0 < o1 && n0 < n1 && old[o0] == new[n0] {
            out.push((n0, o0));
            o0 += 1;
            n0 += 1;
        }
        while o0 < o1 && n0 < n1 && old[o1 - 1] == new[n1 - 1] {
            out.push((n1 - 1, o1 - 1));
            o1 -= 1;
            n1 -= 1;
        }
        if o0 == o1 || n0 == n1 {
            continue;
        }
        // Unique-on-both-sides elements, longest increasing run of them.
        let mut cnt: HashMap<u64, (u32, u32, usize, usize)> = HashMap::new();
        for (i, &v) in old.iter().enumerate().take(o1).skip(o0) {
            let e = cnt.entry(v).or_insert((0, 0, i, 0));
            e.0 += 1;
            e.2 = i;
        }
        for (i, &v) in new.iter().enumerate().take(n1).skip(n0) {
            if let Some(e) = cnt.get_mut(&v) {
                e.1 += 1;
                e.3 = i;
            }
        }
        let mut pairs: Vec<(usize, usize)> =
            cnt.values().filter(|e| e.0 == 1 && e.1 == 1).map(|e| (e.3, e.2)).collect();
        pairs.sort_unstable();
        let lis = longest_increasing(&pairs);
        if !lis.is_empty() {
            let (mut po, mut pn) = (o0, n0);
            for &(n, o) in &lis {
                out.push((n, o));
                work.push((po, o, pn, n));
                po = o + 1;
                pn = n + 1;
            }
            work.push((po, o1, pn, n1));
            continue;
        }
        let (lo, ln) = (o1 - o0, n1 - n0);
        if lo == ln {
            out.extend((0..lo).map(|k| (n0 + k, o0 + k)));
        } else if lo * ln <= 1 << 16 {
            out.extend(lcs(&old[o0..o1], &new[n0..n1]).into_iter().map(|(n, o)| (n0 + n, o0 + o)));
        }
    }
    out
}

/// Longest strictly increasing subsequence (by the second field) of pairs sorted by the first.
fn longest_increasing(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut tails: Vec<usize> = Vec::new(); // indices into pairs
    let mut prev: Vec<Option<usize>> = vec![None; pairs.len()];
    for i in 0..pairs.len() {
        let pos = tails.partition_point(|&t| pairs[t].1 < pairs[i].1);
        if pos > 0 {
            prev[i] = Some(tails[pos - 1]);
        }
        if pos == tails.len() {
            tails.push(i);
        } else {
            tails[pos] = i;
        }
    }
    let mut out = Vec::new();
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        out.push(pairs[i]);
        cur = prev[i];
    }
    out.reverse();
    out
}

/// Classic LCS; returns `(new index, old index)` of the matched elements.
fn lcs(old: &[u64], new: &[u64]) -> Vec<(usize, usize)> {
    let (m, n) = (old.len(), new.len());
    let mut t = vec![0u32; (m + 1) * (n + 1)];
    let at = |i: usize, j: usize| i * (n + 1) + j;
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            t[at(i, j)] = if old[i] == new[j] { t[at(i + 1, j + 1)] + 1 } else { t[at(i + 1, j)].max(t[at(i, j + 1)]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < m && j < n {
        if old[i] == new[j] {
            out.push((j, i));
            i += 1;
            j += 1;
        } else if t[at(i + 1, j)] >= t[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serials() {
        assert_eq!(encode_serial(1024, 3), "100");
        assert_eq!(encode_serial(0, 2), "00");
        assert_eq!(encode_serial(0, 1), "0");
        assert_eq!(encode_serial(32 * 32 * 32 - 1, 3), "zzz");
        assert_eq!(encode_serial(32 * 32 * 32, 3), "1000");
        assert_eq!(encode_serial(32, 1), "10");
    }

    #[test]
    fn grammar_examples() {
        for ok in ["b100", "b1dk", "bzzz", "b1000", "h1-00", "h12-0z", "fn3-1", "c5-0", "tb1-0"] {
            assert!(is_block_anchor(ok), "{ok}");
        }
        for bad in ["b1", "b10", "B1DK", "b1di", "h1", "h0-00", "h01-00", "h1_00", "fn-1", "x1-00", "b-100"] {
            assert!(!is_block_anchor(bad), "{bad}");
        }
        assert!(is_row_anchor("t2.r1") && !is_row_anchor("t2.r0") && !is_row_anchor("t2.r1.c1"));
        assert!(is_cell_anchor("t1.r1.c1") && !is_cell_anchor("t2.c3.r2"));
        assert!(is_story_id("b") && is_story_id("fn3") && !is_story_id("h0"));
    }

    #[test]
    fn diff_pairs_moves_and_edits() {
        let old = [1, 2, 3, 4, 5];
        let new = [1, 9, 2, 3, 7, 5];
        let mut m = diff(&old, &new);
        m.sort_unstable();
        assert_eq!(m, vec![(0, 0), (2, 1), (3, 2), (4, 3), (5, 4)]);
    }
}
