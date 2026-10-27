//! Selections (`common.schema.json#/$defs/Selection`): a comma list of `start`, `start..`,
//! `start..end` and `..end`.

use crate::anchors;
use crate::{Error, Result};

/// One part of a selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sel {
    /// One anchor or story id: that block, table, row, cell, image, comment or story, whole.
    One(String),
    /// From `start` (or the first block) to `end` (or the last), windowed by the budget.
    Range(Option<String>, Option<String>),
}

impl Sel {
    /// The selection text of this part.
    pub fn text(&self) -> String {
        match self {
            Sel::One(a) => a.clone(),
            Sel::Range(a, b) => format!("{}..{}", a.as_deref().unwrap_or(""), b.as_deref().unwrap_or("")),
        }
    }
}

fn valid_start(s: &str) -> bool {
    anchors::is_anchor(s) || anchors::is_story_id(s)
}

fn valid_end(s: &str) -> bool {
    anchors::is_block_anchor(s) || anchors::is_table_anchor(s) || anchors::is_image_anchor(s)
}

/// Parse a selection.
pub(crate) fn parse(s: &str) -> Result<Vec<Sel>> {
    let bad = |why: &str| Error::Anchor(format!("invalid selection {s:?}: {why}"));
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(bad("empty part"));
        }
        match part.split_once("..") {
            None => {
                if !valid_start(part) {
                    return Err(bad(&format!("{part:?} is not an anchor")));
                }
                out.push(Sel::One(part.to_owned()));
            }
            Some((a, b)) => {
                let a = (!a.is_empty()).then(|| a.to_owned());
                let b = (!b.is_empty()).then(|| b.to_owned());
                if a.is_none() && b.is_none() {
                    return Err(bad("`..` needs a start or an end"));
                }
                if let Some(a) = &a
                    && !anchors::is_anchor(a)
                {
                    return Err(bad(&format!("{a:?} can't start a range")));
                }
                if let Some(b) = &b
                    && !valid_end(b)
                {
                    return Err(bad(&format!("{b:?} can't end a range")));
                }
                out.push(Sel::Range(a, b));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(parse("b105").unwrap(), vec![Sel::One("b105".into())]);
        assert_eq!(
            parse("b105..b10h, t1.r5..").unwrap(),
            vec![Sel::Range(Some("b105".into()), Some("b10h".into())), Sel::Range(Some("t1.r5".into()), None)]
        );
        assert_eq!(parse("..b105").unwrap(), vec![Sel::Range(None, Some("b105".into()))]);
        assert_eq!(parse("h1").unwrap(), vec![Sel::One("h1".into())]);
        assert!(parse("..").is_err());
        assert!(parse("b105..t1.r2").is_err());
        assert!(parse("x").is_err());
        assert!(parse("b105,").is_err());
    }
}
