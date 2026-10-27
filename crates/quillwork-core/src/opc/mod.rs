//! The package layer: a safe zip reader, a raw-entry splicer, `[Content_Types].xml` and
//! relationships. See docs/DESIGN.md §3 and §12.
//!
//! A [`Package`] keeps the original file bytes and, per entry, where its local header, data and
//! data descriptor live. Parts are inflated lazily. A save with no changes returns the original
//! bytes; otherwise clean entries are copied verbatim (local header, data, descriptor and
//! central-directory record) and only changed parts are re-compressed.
//!
//! Part names are zip entry names without a leading `/` (`word/document.xml`); every method also
//! accepts the `/word/document.xml` form. Lookups are case-insensitive, as in OPC.

mod content_types;
mod names;
mod xmlscan;
mod zipfmt;

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

pub use content_types::{CONTENT_TYPES_PART, ContentTypes};
pub use names::{pct_decode, pct_encode, relative_target, rels_part_for, resolve_target, source_of_rels};

use names::norm;
use zipfmt::RawEntry;

use crate::{Error, Result};

/// Content type of relationship parts.
pub const RELS_CONTENT_TYPE: &str = "application/vnd.openxmlformats-package.relationships+xml";
const RELS_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const OFFICE_DOCUMENT_REL: &str = "/officeDocument";

/// Resource limits enforced while reading a package (DESIGN §12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Sum of all entries' uncompressed sizes.
    pub max_total_uncompressed: u64,
    /// Largest allowed uncompressed size of one entry.
    pub max_part_size: u64,
    /// Largest allowed uncompressed:compressed ratio of one entry.
    pub max_ratio: u64,
    /// Entries at or below this uncompressed size are exempt from the ratio check (tiny,
    /// repetitive XML legitimately compresses very well; the total cap still bounds them).
    pub ratio_min_size: u64,
    /// Largest number of zip entries.
    pub max_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_total_uncompressed: 200 << 20,
            max_part_size: 100 << 20,
            max_ratio: 100,
            ratio_min_size: 1 << 20,
            max_entries: 10_000,
        }
    }
}

/// A relationship from a `.rels` part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relationship {
    pub id: String,
    pub rel_type: String,
    /// The target exactly as written (relative URI, or a URL when external).
    pub target: String,
    /// `TargetMode="External"`: never resolved to a part, never fetched.
    pub external: bool,
}

/// Read-only facts about one entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryInfo {
    pub name: String,
    /// Zip compression method (0 stored, 8 deflate).
    pub method: u16,
    /// General-purpose flags from the central directory.
    pub flags: u16,
    pub crc32: u32,
    /// Compressed size in the original file (`None` for parts added this session).
    pub compressed_size: Option<u64>,
    pub uncompressed_size: u64,
    /// MS-DOS time and date.
    pub mod_time: u16,
    pub mod_date: u16,
    pub has_data_descriptor: bool,
    /// Content differs from the original file.
    pub dirty: bool,
    /// Added this session.
    pub added: bool,
}

struct Entry {
    name: String,
    /// The original entry; `None` for parts added this session.
    raw: Option<RawEntry>,
    /// New content: `Some` for changed and added parts.
    data: Option<Vec<u8>>,
    /// Inflated original content.
    cache: OnceLock<Vec<u8>>,
    /// Deflate level to re-compress with, guessed when the part was first changed.
    level: Option<u32>,
}

impl Entry {
    fn size(&self) -> u64 {
        match (&self.data, &self.raw) {
            (Some(d), _) => d.len() as u64,
            (None, Some(r)) => r.uncompressed_size,
            (None, None) => 0,
        }
    }
}

/// An open OPC package.
pub struct Package {
    original: Vec<u8>,
    comment: std::ops::Range<usize>,
    entries: Vec<Entry>,
    /// Lower-cased name → index into `entries`.
    index: HashMap<String, usize>,
    limits: Limits,
    /// An original entry was removed.
    removed_original: bool,
    /// Names of parts added this session (lower-cased).
    added: BTreeSet<String>,
    content_types: ContentTypes,
    total: u64,
    /// Version-made-by / time / date that new entries copy (from the first entry), so output
    /// is deterministic and looks like the rest of the file.
    template: (u16, u16, u16),
}

impl std::fmt::Debug for Package {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Package")
            .field("bytes", &self.original.len())
            .field("entries", &self.entries.len())
            .field("modified", &self.is_modified())
            .finish()
    }
}

/// Content type for a media extension.
pub fn media_content_type(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "emf" => "image/x-emf",
        "wmf" => "image/x-wmf",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        _ => return None,
    })
}

fn not_found(name: &str) -> Error {
    Error::NotFound(format!("part {name:?}"))
}

impl Package {
    /// Open a package, validating its structure against `limits`. Nothing is inflated except
    /// `[Content_Types].xml`.
    pub fn open(bytes: Vec<u8>, limits: &Limits) -> Result<Package> {
        let archive = zipfmt::parse(&bytes, limits)?;
        let template =
            archive.entries.first().map(|e| (e.version_made_by, e.mod_time, e.mod_date)).unwrap_or((20, 0, 0x21));
        let mut index = HashMap::with_capacity(archive.entries.len());
        let mut total = 0u64;
        let entries: Vec<Entry> = archive
            .entries
            .into_iter()
            .enumerate()
            .map(|(i, raw)| {
                index.insert(raw.name.to_lowercase(), i);
                total += raw.uncompressed_size;
                Entry { name: raw.name.clone(), raw: Some(raw), data: None, cache: OnceLock::new(), level: None }
            })
            .collect();
        let mut pkg = Package {
            original: bytes,
            comment: archive.comment,
            entries,
            index,
            limits: limits.clone(),
            removed_original: false,
            added: BTreeSet::new(),
            content_types: ContentTypes::default(),
            total,
            template,
        };
        let ct = pkg.part(CONTENT_TYPES_PART).map_err(|e| match e {
            Error::NotFound(_) => Error::Package("missing [Content_Types].xml".into()),
            e => e,
        })?;
        pkg.content_types = ContentTypes::parse(&ct)?;
        Ok(pkg)
    }

    /// The bytes the package was opened from.
    pub fn original_bytes(&self) -> &[u8] {
        &self.original
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Entry names in zip order (new parts last).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.index.get(&norm(name).to_lowercase()).copied()
    }

    /// Whether a save would differ from the original bytes.
    pub fn is_modified(&self) -> bool {
        self.removed_original || self.entries.iter().any(|e| e.data.is_some())
    }

    pub fn entry_info(&self, name: &str) -> Option<EntryInfo> {
        let e = &self.entries[self.find(name)?];
        let dirty = e.data.is_some();
        Some(match &e.raw {
            Some(r) => EntryInfo {
                name: e.name.clone(),
                method: r.method,
                flags: r.flags,
                crc32: match &e.data {
                    Some(d) => crc32fast::hash(d),
                    None => r.crc32,
                },
                compressed_size: Some(r.compressed_size),
                uncompressed_size: e.size(),
                mod_time: r.mod_time,
                mod_date: r.mod_date,
                has_data_descriptor: r.has_descriptor,
                dirty,
                added: false,
            },
            None => EntryInfo {
                name: e.name.clone(),
                method: zipfmt::METHOD_DEFLATE,
                flags: 0,
                crc32: e.data.as_deref().map(crc32fast::hash).unwrap_or(0),
                compressed_size: None,
                uncompressed_size: e.size(),
                mod_time: self.template.1,
                mod_date: self.template.2,
                has_data_descriptor: false,
                dirty: true,
                added: true,
            },
        })
    }

    fn original_content(&self, raw: &RawEntry, cache: &'_ OnceLock<Vec<u8>>) -> Result<()> {
        if cache.get().is_none() {
            let v = zipfmt::inflate(&self.original, raw)?;
            let _ = cache.set(v);
        }
        Ok(())
    }

    /// A part's content (inflated and CRC-checked on first access, then cached).
    pub fn part(&self, name: &str) -> Result<Cow<'_, [u8]>> {
        let i = self.find(name).ok_or_else(|| not_found(name))?;
        let e = &self.entries[i];
        if let Some(d) = &e.data {
            return Ok(Cow::Borrowed(d));
        }
        let Some(raw) = &e.raw else { return Ok(Cow::Borrowed(&[])) };
        if raw.method == zipfmt::METHOD_STORED {
            let d = &self.original[raw.data.clone()];
            if crc32fast::hash(d) != raw.crc32 {
                return Err(Error::Package(format!("entry {:?}: crc mismatch", raw.name)));
            }
            return Ok(Cow::Borrowed(d));
        }
        self.original_content(raw, &e.cache)?;
        Ok(Cow::Borrowed(e.cache.get().map(Vec::as_slice).unwrap_or(&[])))
    }

    /// A part's content as UTF-8 text.
    pub fn part_str(&self, name: &str) -> Result<Cow<'_, str>> {
        match self.part(name)? {
            Cow::Borrowed(b) => std::str::from_utf8(b)
                .map(Cow::Borrowed)
                .map_err(|e| Error::Package(format!("part {name:?} is not UTF-8: {e}"))),
            Cow::Owned(b) => String::from_utf8(b)
                .map(Cow::Owned)
                .map_err(|e| Error::Package(format!("part {name:?} is not UTF-8: {e}"))),
        }
    }

    fn check_size(&self, old: u64, new: u64) -> Result<()> {
        if new > self.limits.max_part_size {
            return Err(Error::Limit(format!("part of {new} bytes (limit {})", self.limits.max_part_size)));
        }
        if self.total - old + new > self.limits.max_total_uncompressed {
            return Err(Error::Limit(format!(
                "package would exceed {} bytes uncompressed",
                self.limits.max_total_uncompressed
            )));
        }
        Ok(())
    }

    /// Replace a part's content. Setting a part to its current content is a no-op, and setting
    /// it back to its original content makes it clean again.
    pub fn set_part(&mut self, name: &str, bytes: Vec<u8>) -> Result<()> {
        let i = self.find(name).ok_or_else(|| not_found(name))?;
        self.check_size(self.entries[i].size(), bytes.len() as u64)?;
        let is_ct = self.entries[i].name == CONTENT_TYPES_PART;
        let new_ct = if is_ct { Some(ContentTypes::parse(&bytes)?) } else { None };

        // Back to (or still at) the original content?
        let mut back_to_original = false;
        let mut level = self.entries[i].level;
        if let Some(raw) = &self.entries[i].raw {
            if raw.uncompressed_size == bytes.len() as u64 && raw.crc32 == crc32fast::hash(&bytes) {
                let orig = if raw.method == zipfmt::METHOD_STORED {
                    Cow::Borrowed(&self.original[raw.data.clone()])
                } else {
                    Cow::Owned(zipfmt::inflate(&self.original, raw)?)
                };
                back_to_original = *orig == bytes[..];
            }
            if !back_to_original && level.is_none() && raw.method == zipfmt::METHOD_DEFLATE {
                let plain = if self.entries[i].cache.get().is_some() {
                    None
                } else {
                    zipfmt::inflate(&self.original, raw).ok()
                };
                let plain = self.entries[i].cache.get().map(Vec::as_slice).or(plain.as_deref());
                level = Some(zipfmt::guess_level(plain, &self.original[raw.data.clone()], raw.flags));
            }
        } else if self.entries[i].data.as_deref() == Some(&bytes[..]) {
            return Ok(());
        }

        let old = self.entries[i].size();
        let e = &mut self.entries[i];
        e.level = level;
        e.cache = OnceLock::new();
        e.data = if back_to_original { None } else { Some(bytes) };
        let new = e.size();
        self.total = self.total - old + new;
        if let Some(ct) = new_ct {
            self.content_types = ct;
        }
        Ok(())
    }

    /// Add a new part (appended after all existing entries) and register its content type:
    /// nothing if the extension's Default already gives `content_type`, else an Override.
    pub fn add_part(&mut self, name: &str, bytes: Vec<u8>, content_type: &str) -> Result<()> {
        let name = norm(name);
        zipfmt::validate_name(name, false)?;
        if name.ends_with('/') {
            return Err(Error::Package(format!("{name:?} is not a part name")));
        }
        if self.contains(name) {
            return Err(Error::Package(format!("part {name:?} already exists")));
        }
        if source_of_rels(name).is_none() && content_type.is_empty() {
            return Err(Error::Package(format!("part {name:?} needs a content type")));
        }
        if self.entries.len() >= self.limits.max_entries {
            return Err(Error::Limit(format!("more than {} entries", self.limits.max_entries)));
        }
        self.check_size(0, bytes.len() as u64)?;
        self.total += bytes.len() as u64;
        self.index.insert(name.to_lowercase(), self.entries.len());
        self.added.insert(name.to_lowercase());
        self.entries.push(Entry {
            name: name.to_owned(),
            raw: None,
            data: Some(bytes),
            cache: OnceLock::new(),
            level: None,
        });
        if !content_type.is_empty() && self.content_types.content_type_of(name) != Some(content_type) {
            self.register_override(name, content_type)?;
        }
        Ok(())
    }

    /// Remove a part and its content-type Override. Relationships pointing at it are left
    /// alone (the caller owns them).
    pub fn remove_part(&mut self, name: &str) -> Result<()> {
        let name = norm(name);
        if name == CONTENT_TYPES_PART {
            return Err(Error::Package("cannot remove [Content_Types].xml".into()));
        }
        let i = self.find(name).ok_or_else(|| not_found(name))?;
        let e = self.entries.remove(i);
        self.total -= e.size();
        if e.raw.is_some() {
            self.removed_original = true;
        }
        self.added.remove(&e.name.to_lowercase());
        self.index = self.entries.iter().enumerate().map(|(i, e)| (e.name.to_lowercase(), i)).collect();
        self.remove_override(&e.name)?;
        Ok(())
    }

    // ---- content types ----

    pub fn content_types(&self) -> &ContentTypes {
        &self.content_types
    }

    pub fn content_type_of(&self, part: &str) -> Option<&str> {
        self.content_types.content_type_of(part)
    }

    fn ct_scan(&self) -> Result<(Vec<u8>, xmlscan::Scan)> {
        let xml = self.part(CONTENT_TYPES_PART)?.into_owned();
        let scan = xmlscan::scan(&xml, "Types")?;
        Ok((xml, scan))
    }

    /// Register (or change) the Override for `part`. Unchanged entries keep their bytes; a new
    /// element is appended before `</Types>`.
    pub fn register_override(&mut self, part: &str, content_type: &str) -> Result<()> {
        let part = norm(part);
        if self.content_types.override_for(part) == Some(content_type) {
            return Ok(());
        }
        let (mut xml, mut scan) = self.ct_scan()?;
        if let Some(c) = scan.children.iter().rev().find(|c| is_override_for(c, part)) {
            xml = xmlscan::remove(&xml, c.span.clone());
            scan = xmlscan::scan(&xml, "Types")?;
        }
        let frag = format!(
            "<{p}Override PartName=\"/{}\" ContentType=\"{}\"/>",
            xmlscan::escape_attr(&pct_encode(part)),
            xmlscan::escape_attr(content_type),
            p = scan.child_prefix()
        );
        let xml = scan.insert(&xml, &frag);
        self.set_part(CONTENT_TYPES_PART, xml)
    }

    /// Register a Default for an extension. A no-op if the same Default exists; an error if a
    /// different one does (changing it would retype existing parts).
    pub fn register_default(&mut self, ext: &str, content_type: &str) -> Result<()> {
        match self.content_types.default_for(ext) {
            Some(t) if t == content_type => return Ok(()),
            Some(t) => {
                return Err(Error::Package(format!("extension {ext:?} already defaults to {t:?}")));
            }
            None => {}
        }
        let (xml, scan) = self.ct_scan()?;
        let frag = format!(
            "<{p}Default Extension=\"{}\" ContentType=\"{}\"/>",
            xmlscan::escape_attr(ext),
            xmlscan::escape_attr(content_type),
            p = scan.child_prefix()
        );
        let xml = scan.insert(&xml, &frag);
        self.set_part(CONTENT_TYPES_PART, xml)
    }

    fn remove_override(&mut self, part: &str) -> Result<()> {
        if self.content_types.override_for(part).is_none() {
            return Ok(());
        }
        let (mut xml, _) = self.ct_scan()?;
        loop {
            let scan = xmlscan::scan(&xml, "Types")?;
            match scan.children.iter().find(|c| is_override_for(c, part)) {
                Some(c) => xml = xmlscan::remove(&xml, c.span.clone()),
                None => break,
            }
        }
        self.set_part(CONTENT_TYPES_PART, xml)
    }

    // ---- relationships ----

    /// The relationships of `source` (`""` for the package-level `_rels/.rels`). Empty when it
    /// has no rels part.
    pub fn rels_of(&self, source: &str) -> Result<Vec<Relationship>> {
        let rels = rels_part_for(source);
        if !self.contains(&rels) {
            return Ok(Vec::new());
        }
        let xml = self.part(&rels)?;
        let scan = xmlscan::scan(&xml, "Relationships")?;
        Ok(scan
            .children
            .iter()
            .filter(|c| c.local == "Relationship")
            .map(|c| Relationship {
                id: c.attr("Id").unwrap_or_default().to_owned(),
                rel_type: c.attr("Type").unwrap_or_default().to_owned(),
                target: c.attr("Target").unwrap_or_default().to_owned(),
                external: c.attr("TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("External")),
            })
            .collect())
    }

    /// The part a relationship points at, or `None` for external targets (which are never
    /// resolved or fetched) and targets outside the package.
    pub fn resolve(&self, source: &str, rel: &Relationship) -> Option<String> {
        if rel.external {
            return None;
        }
        resolve_target(source, &rel.target)
    }

    /// Add a relationship from `source` and return its id (`rIdQw<n>`, unused in that rels
    /// part). Creates the rels part if needed. `target` is written as given: a relative URI for
    /// internal targets (see [`relative_target`]) or a URL when `external`.
    pub fn add_relationship(&mut self, source: &str, rel_type: &str, target: &str, external: bool) -> Result<String> {
        let source = norm(source);
        if !source.is_empty() && !self.contains(source) {
            return Err(not_found(source));
        }
        let rels = rels_part_for(source);
        let exists = self.contains(&rels);
        let xml = if exists {
            self.part(&rels)?.into_owned()
        } else {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<Relationships xmlns=\"{RELS_NS}\"></Relationships>"
            )
            .into_bytes()
        };
        let scan = xmlscan::scan(&xml, "Relationships")?;
        let used: HashSet<&str> = scan.children.iter().filter_map(|c| c.attr("Id")).collect();
        let id = (1u64..).map(|n| format!("rIdQw{n}")).find(|id| !used.contains(id.as_str())).unwrap_or_default();
        let frag = format!(
            "<{p}Relationship Id=\"{id}\" Type=\"{}\" Target=\"{}\"{}/>",
            xmlscan::escape_attr(rel_type),
            xmlscan::escape_attr(target),
            if external { " TargetMode=\"External\"" } else { "" },
            p = scan.child_prefix()
        );
        let xml = scan.insert(&xml, &frag);
        if exists {
            self.set_part(&rels, xml)?;
        } else {
            self.add_part(&rels, xml, RELS_CONTENT_TYPE)?;
        }
        Ok(id)
    }

    /// Add an internal relationship from `source` to the part `target_part`.
    pub fn add_relationship_to_part(&mut self, source: &str, rel_type: &str, target_part: &str) -> Result<String> {
        if !self.contains(target_part) {
            return Err(not_found(target_part));
        }
        let target = relative_target(source, target_part);
        self.add_relationship(source, rel_type, &target, false)
    }

    // ---- media ----

    /// Where new media goes: `media/` next to the main document (`word/media/`), else `media/`.
    pub fn media_dir(&self) -> String {
        let main = self.rels_of("").ok().and_then(|rels| {
            rels.iter()
                .find(|r| !r.external && r.rel_type.ends_with(OFFICE_DOCUMENT_REL))
                .and_then(|r| resolve_target("", &r.target))
        });
        match main.as_deref().and_then(|m| m.rsplit_once('/')) {
            Some((dir, _)) => format!("{dir}/media/"),
            None => "media/".to_owned(),
        }
    }

    /// Add media, or return the existing part holding exactly these bytes. Candidates are found
    /// by size + CRC-32 (from the central directory, so nothing is inflated) and confirmed by a
    /// full byte comparison.
    pub fn add_media(&mut self, bytes: Vec<u8>, ext: &str) -> Result<String> {
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        let ct = media_content_type(&ext).ok_or_else(|| Error::Package(format!("unknown media extension {ext:?}")))?;
        let crc = crc32fast::hash(&bytes);
        let len = bytes.len() as u64;
        let suffix = format!(".{ext}");
        let candidates: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.name.to_ascii_lowercase().ends_with(&suffix) && e.size() == len)
            .filter(|e| match (&e.data, &e.raw) {
                (Some(d), _) => crc32fast::hash(d) == crc,
                (None, Some(r)) => r.crc32 == crc,
                _ => false,
            })
            .map(|e| e.name.clone())
            .collect();
        for name in candidates {
            if self.part(&name).is_ok_and(|p| *p == bytes[..]) {
                return Ok(name);
            }
        }
        let dir = self.media_dir();
        let name = (1u64..).map(|n| format!("{dir}imageQw{n}.{ext}")).find(|n| !self.contains(n)).unwrap_or_default();
        if self.content_types.default_for(&ext).is_none() {
            self.register_default(&ext, ct)?;
        }
        self.add_part(&name, bytes, ct)?;
        Ok(name)
    }

    // ---- garbage collection ----

    /// Remove parts *added this session* that no relationship reaches any more (with their own
    /// rels parts), repeating until nothing changes. Pre-existing parts are never touched.
    /// Returns the removed part names.
    pub fn gc_orphans(&mut self) -> Result<Vec<String>> {
        let mut removed = Vec::new();
        loop {
            let mut referenced: HashSet<String> = HashSet::new();
            let rels_parts: Vec<(String, String)> =
                self.entries.iter().filter_map(|e| source_of_rels(&e.name).map(|s| (e.name.clone(), s))).collect();
            for (_, source) in &rels_parts {
                if !source.is_empty() && !self.contains(source) {
                    continue; // a rels part whose source is gone references nothing
                }
                for rel in self.rels_of(source)? {
                    if let Some(t) = self.resolve(source, &rel) {
                        referenced.insert(t.to_lowercase());
                    }
                }
            }
            let orphans: Vec<String> = self
                .entries
                .iter()
                .filter(|e| {
                    let lc = e.name.to_lowercase();
                    self.added.contains(&lc) && source_of_rels(&e.name).is_none() && !referenced.contains(&lc)
                })
                .map(|e| e.name.clone())
                .collect();
            if orphans.is_empty() {
                break;
            }
            for name in orphans {
                self.remove_part(&name)?;
                let rels = rels_part_for(&name);
                if self.added.contains(&rels.to_lowercase()) {
                    self.remove_part(&rels)?;
                    removed.push(rels);
                }
                removed.push(name);
            }
        }
        Ok(removed)
    }

    // ---- save ----

    /// Serialise the package. Unmodified: the original bytes exactly. Modified: every clean
    /// entry's local header, data, descriptor and central record are copied verbatim (only the
    /// offset changes), changed parts are re-compressed with their original method and
    /// (guessed) level, new parts are appended, and the archive comment is kept.
    pub fn save(&self) -> Result<Vec<u8>> {
        if !self.is_modified() {
            return Ok(self.original.clone());
        }
        self.save_rebuilt()
    }

    /// Always run the splicer, even with no changes (used to test it).
    #[doc(hidden)]
    pub fn save_rebuilt(&self) -> Result<Vec<u8>> {
        let mut w = zipfmt::Writer::new(&self.original, self.original.len() + 4096);
        for e in &self.entries {
            match (&e.raw, &e.data) {
                (Some(raw), None) => w.copy_clean(raw)?,
                (Some(raw), Some(d)) => {
                    let crc = crc32fast::hash(d);
                    if raw.method == zipfmt::METHOD_STORED {
                        w.write_dirty(raw, zipfmt::METHOD_STORED, crc, d.len() as u64, d)?;
                    } else {
                        // Deflate keeps its level; anything else we cannot write becomes deflate.
                        let comp = zipfmt::deflate(d, e.level.unwrap_or(6));
                        w.write_dirty(raw, zipfmt::METHOD_DEFLATE, crc, d.len() as u64, &comp)?;
                    }
                }
                (None, Some(d)) => {
                    let comp = zipfmt::deflate(d, 6);
                    w.write_new(&zipfmt::NewEntry {
                        name: &e.name,
                        method: zipfmt::METHOD_DEFLATE,
                        crc32: crc32fast::hash(d),
                        uncompressed_size: d.len() as u64,
                        compressed: &comp,
                        version_made_by: self.template.0,
                        mod_time: self.template.1,
                        mod_date: self.template.2,
                    })?;
                }
                (None, None) => {}
            }
        }
        w.finish(&self.original[self.comment.clone()])
    }
}

fn is_override_for(c: &xmlscan::Child, part: &str) -> bool {
    c.local == "Override"
        && c.attr("PartName").is_some_and(|p| pct_decode(norm(p)).to_lowercase() == part.to_lowercase())
}

#[cfg(test)]
mod tests;
