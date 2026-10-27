//! `[Content_Types].xml`: Default (by extension) and Override (by part name) maps.

use super::names::{norm, pct_decode};
use super::xmlscan;
use crate::Result;

pub const CONTENT_TYPES_PART: &str = "[Content_Types].xml";

/// The parsed content-type map. Part names are stored without the leading `/`, percent-decoded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentTypes {
    defaults: Vec<(String, String)>,
    overrides: Vec<(String, String)>,
}

impl ContentTypes {
    pub fn parse(xml: &[u8]) -> Result<Self> {
        let scan = xmlscan::scan(xml, "Types")?;
        let mut ct = ContentTypes::default();
        for c in &scan.children {
            match c.local.as_str() {
                "Default" => {
                    if let (Some(ext), Some(t)) = (c.attr("Extension"), c.attr("ContentType")) {
                        ct.defaults.push((ext.to_owned(), t.to_owned()));
                    }
                }
                "Override" => {
                    if let (Some(p), Some(t)) = (c.attr("PartName"), c.attr("ContentType")) {
                        ct.overrides.push((pct_decode(norm(p)), t.to_owned()));
                    }
                }
                _ => {}
            }
        }
        Ok(ct)
    }

    /// `(extension, content type)` pairs in file order.
    pub fn defaults(&self) -> &[(String, String)] {
        &self.defaults
    }

    /// `(part name, content type)` pairs in file order.
    pub fn overrides(&self) -> &[(String, String)] {
        &self.overrides
    }

    /// The Default for an extension (case-insensitive).
    pub fn default_for(&self, ext: &str) -> Option<&str> {
        self.defaults.iter().find(|(e, _)| e.eq_ignore_ascii_case(ext)).map(|(_, t)| t.as_str())
    }

    /// The Override for a part (case-insensitive, leading `/` optional).
    pub fn override_for(&self, part: &str) -> Option<&str> {
        let part = norm(part);
        self.overrides.iter().find(|(p, _)| p.to_lowercase() == part.to_lowercase()).map(|(_, t)| t.as_str())
    }

    /// The effective content type of a part: its Override, else the Default for its extension.
    pub fn content_type_of(&self, part: &str) -> Option<&str> {
        let part = norm(part);
        self.override_for(part).or_else(|| {
            let file = part.rsplit('/').next().unwrap_or(part);
            let ext = file.rsplit_once('.').map(|(_, e)| e)?;
            self.default_for(ext)
        })
    }
}
