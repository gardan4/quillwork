//! Tracked-change writing: revision ids, dates, the word-level diff and the XML writers that turn
//! an edit into `w:ins` / `w:del` / `w:rPrChange` / `w:pPrChange` markup (DESIGN §7).
//!
//! Everything here edits only the nodes it is given; untouched XML keeps its source bytes.

pub mod diff;
pub mod ids;
pub mod write;

use crate::Result;
use crate::xml::{self, NodeId, ns};

pub use ids::IdAlloc;

/// Word 2023 UTC revision dates (`w16du:dateUtc`).
pub const W16DU: &str = "http://schemas.microsoft.com/office/word/2023/wordml/word16du";

/// Who and when a revision is stamped with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub author: String,
    /// `YYYY-MM-DDTHH:MM:SSZ`, written to both `w:date` and `w16du:dateUtc`.
    pub date: String,
}

impl Stamp {
    pub fn new(author: impl Into<String>, unix_secs: i64) -> Stamp {
        Stamp { author: author.into(), date: iso_utc(unix_secs) }
    }
}

/// Seconds since the Unix epoch. On wasm32 there is no clock: 0 (hosts call `Engine::set_now`).
pub fn now_secs() -> i64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time.
pub fn iso_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (H. Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

/// Declare `uri` on the part's root (as `want`, or `{want}q{n}` when that prefix is taken), make
/// sure `mc` is declared, and list the prefix in `mc:Ignorable`. Returns the prefix. A part that
/// already declares and ignores it is left untouched.
pub fn ensure_ignorable_ns(doc: &mut xml::Document, uri: &str, want: &str) -> Result<String> {
    let root = doc.root();
    let prefix = match doc.find_prefix(root, uri).filter(|p| !p.is_empty()) {
        Some(p) => p.to_owned(),
        None => {
            let p = free_prefix(doc, root, want);
            doc.set_attr(root, &format!("xmlns:{p}"), uri)?;
            p
        }
    };
    if doc.find_prefix(root, ns::MC).filter(|p| !p.is_empty()).is_none() {
        let p = free_prefix(doc, root, "mc");
        doc.set_attr(root, &format!("xmlns:{p}"), ns::MC)?;
    }
    let current = doc.attr_ns(root, ns::MC, "Ignorable").map(|c| c.into_owned());
    match current {
        Some(v) if v.split_ascii_whitespace().any(|t| t == prefix) => {}
        Some(v) if v.trim().is_empty() => doc.set_attr_ns(root, ns::MC, "Ignorable", &prefix)?,
        Some(v) => doc.set_attr_ns(root, ns::MC, "Ignorable", &format!("{v} {prefix}"))?,
        None => doc.set_attr_ns(root, ns::MC, "Ignorable", &prefix)?,
    }
    Ok(prefix)
}

fn free_prefix(doc: &xml::Document, n: NodeId, want: &str) -> String {
    if doc.lookup_namespace(n, want).is_none() {
        return want.to_owned();
    }
    (1..).map(|i| format!("{want}q{i}")).find(|p| doc.lookup_namespace(n, p).is_none()).expect("unbounded")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso_utc(1_793_016_000), "2026-10-26T12:00:00Z");
    }
}
