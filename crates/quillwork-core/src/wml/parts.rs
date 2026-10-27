//! Where `WordDoc` gets its parts from: a minimal trait over the package, so the Word model does
//! not care whether the bytes come from an [`opc::Package`] or an in-memory map (tests, fixtures).

use std::borrow::Cow;
use std::collections::HashMap;

use crate::opc::{self, Package, Relationship};
use crate::xml::{self, ns};
use crate::{Error, Result};

/// Read access to the parts of a package. Part names have no leading `/` (`word/document.xml`)
/// and are matched case-insensitively.
pub trait Parts {
    /// Every part name in the package.
    fn part_names(&self) -> Vec<String>;
    /// A part's bytes. `Error::NotFound` when absent.
    fn part(&self, name: &str) -> Result<Cow<'_, [u8]>>;
    /// Relationships of `source` (`""` = package-level `_rels/.rels`), targets as written.
    /// Empty when the part has no rels part.
    fn rels(&self, source: &str) -> Result<Vec<Relationship>>;
}

impl Parts for Package {
    fn part_names(&self) -> Vec<String> {
        self.names().map(str::to_owned).collect()
    }
    fn part(&self, name: &str) -> Result<Cow<'_, [u8]>> {
        Package::part(self, name)
    }
    fn rels(&self, source: &str) -> Result<Vec<Relationship>> {
        self.rels_of(source)
    }
}

/// An in-memory package: part name → bytes. Used by tests and for small hand-made fixtures.
#[derive(Debug, Clone, Default)]
pub struct MemParts {
    parts: Vec<(String, Vec<u8>)>,
    index: HashMap<String, usize>,
}

impl MemParts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from (name, bytes) pairs, e.g. the entries of an unzipped docx.
    pub fn from_parts<I, S>(parts: I) -> Self
    where
        I: IntoIterator<Item = (S, Vec<u8>)>,
        S: Into<String>,
    {
        let mut m = Self::new();
        for (n, b) in parts {
            m.insert(n, b);
        }
        m
    }

    /// Add or replace a part.
    pub fn insert(&mut self, name: impl Into<String>, bytes: Vec<u8>) {
        let name: String = name.into();
        let name = name.strip_prefix('/').map(str::to_owned).unwrap_or(name);
        let key = name.to_lowercase();
        match self.index.get(&key) {
            Some(&i) => self.parts[i].1 = bytes,
            None => {
                self.index.insert(key, self.parts.len());
                self.parts.push((name, bytes));
            }
        }
    }
}

impl Parts for MemParts {
    fn part_names(&self) -> Vec<String> {
        self.parts.iter().map(|(n, _)| n.clone()).collect()
    }

    fn part(&self, name: &str) -> Result<Cow<'_, [u8]>> {
        let key = name.strip_prefix('/').unwrap_or(name).to_lowercase();
        self.index
            .get(&key)
            .map(|&i| Cow::Borrowed(self.parts[i].1.as_slice()))
            .ok_or_else(|| Error::NotFound(format!("part {name:?}")))
    }

    fn rels(&self, source: &str) -> Result<Vec<Relationship>> {
        let rels_name = opc::rels_part_for(source);
        let bytes = match Parts::part(self, &rels_name) {
            Ok(b) => b,
            Err(Error::NotFound(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let doc = xml::Document::parse(&bytes)?;
        Ok(doc
            .child_elements(doc.root())
            .filter(|&c| doc.is(c, ns::REL, "Relationship"))
            .map(|c| Relationship {
                id: doc.attr(c, "Id").unwrap_or_default().into_owned(),
                rel_type: doc.attr(c, "Type").unwrap_or_default().into_owned(),
                target: doc.attr(c, "Target").unwrap_or_default().into_owned(),
                external: doc.attr(c, "TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("External")),
            })
            .collect())
    }
}
