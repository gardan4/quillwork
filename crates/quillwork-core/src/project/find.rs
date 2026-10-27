//! `find`: search the document's text model (spec/projection.md §10–§11) and return anchors with
//! UTF-16 offsets, a snippet and the matching blocks' projection lines.

use std::collections::BTreeSet;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::anchors::Anchors;
use crate::wml::WordDoc;
use crate::wml::style::StyleSheet;
use crate::{Error, Result};

use super::ctx::{DocCtx, DocIndex};
use super::inline::{self, Atom, InlineState};
use super::{ProjectOptions, View, render};

/// Hits returned when the request doesn't say.
pub const DEFAULT_FIND_LIMIT: usize = 50;
/// Largest `limit` a caller may ask for.
pub const MAX_FIND_LIMIT: usize = 500;
/// Characters of context on each side of a hit in its snippet.
const SNIPPET_CONTEXT: usize = 40;

fn default_limit() -> usize {
    DEFAULT_FIND_LIMIT
}

/// A `find` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FindRequest {
    pub query: String,
    /// `query` is a regular expression (Rust `regex` syntax, no look-around or backreferences).
    pub regex: bool,
    pub case_sensitive: bool,
    /// Only matches bounded by word boundaries on both sides.
    pub whole_word: bool,
    /// Limit to one story (`b`, `h1`, `fn2`, `c3`, …); `None` searches every story.
    pub story: Option<String>,
    /// `accepted` searches the accepted text; `markup` also matches deleted text.
    pub view: View,
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub options: ProjectOptions,
}

impl Default for FindRequest {
    fn default() -> Self {
        FindRequest {
            query: String::new(),
            regex: false,
            case_sensitive: false,
            whole_word: false,
            story: None,
            view: View::Accepted,
            limit: DEFAULT_FIND_LIMIT,
            options: ProjectOptions::default(),
        }
    }
}

/// One match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindHit {
    /// The paragraph holding the match.
    pub anchor: String,
    pub story: String,
    /// UTF-16 offsets into the paragraph's text (text model of the requested view).
    pub start: usize,
    pub end: usize,
    /// The matched text.
    pub text: String,
    /// The match with up to 40 characters of context per side, `…` where cut.
    pub snippet: String,
    /// The match touches tracked-deleted text (markup view only).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
}

/// What [`find`] returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindResult {
    /// The first `limit` hits in document order (body, then the other stories).
    pub hits: Vec<FindHit>,
    /// Every match, including those past `limit`.
    pub total: usize,
    /// Projection lines of the blocks holding the returned hits, in hit order.
    pub lines: Vec<String>,
}

impl FindResult {
    /// The result as projection text (spec/projection.md §11): a `§ find` line, then `lines`.
    pub fn text(&self, query: &str) -> String {
        let q = query.replace('\\', "\\\\").replace('"', "\\\"");
        let mut s = format!("§ find \"{q}\" · {} hit{}", self.total, if self.total == 1 { "" } else { "s" });
        if self.hits.len() < self.total {
            s.push_str(&format!(" (showing {})", self.hits.len()));
        }
        s.push('\n');
        for l in &self.lines {
            s.push_str(l);
            s.push('\n');
        }
        s
    }
}

fn build_regex(req: &FindRequest) -> Result<Regex> {
    if req.query.is_empty() {
        return Err(Error::Other("find: empty query".into()));
    }
    if req.query.chars().count() > 1000 {
        return Err(Error::Limit("find: query longer than 1000 characters".into()));
    }
    let mut pat = if req.regex { req.query.clone() } else { regex::escape(&req.query) };
    if req.whole_word {
        pat = format!(r"\b(?:{pat})\b");
    }
    RegexBuilder::new(&pat)
        .case_insensitive(!req.case_sensitive)
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 22)
        .build()
        .map_err(|e| Error::Other(format!("find: invalid pattern: {e}")))
}

/// One paragraph's text in the requested view, with the byte ranges that are tracked deletions.
fn para_text(cx: &DocCtx, pieces: &[inline::Piece], markup: bool) -> (String, Vec<(usize, usize)>) {
    let mut s = String::new();
    let mut del = Vec::new();
    for x in pieces {
        let deleted = x.rev.as_ref().is_some_and(|r| r.kind.is_del());
        if deleted {
            if !markup {
                continue;
            }
            let t = match &x.atom {
                Atom::Field(f) => f.result.clone(),
                a => inline::atom_plain(a),
            };
            let b = s.len();
            s.push_str(&t);
            del.push((b, s.len()));
        } else {
            s.push_str(&x.plain);
        }
    }
    let _ = cx;
    (s, del)
}

fn utf16_at(s: &str, byte: usize) -> usize {
    s[..byte].encode_utf16().count()
}

fn snippet(s: &str, b: usize, e: usize) -> String {
    let before: Vec<char> = s[..b].chars().collect();
    let after: Vec<char> = s[e..].chars().collect();
    let lo = before.len().saturating_sub(SNIPPET_CONTEXT);
    let mut out = String::new();
    if lo > 0 {
        out.push('…');
    }
    out.extend(&before[lo..]);
    out.push_str(&s[b..e]);
    out.extend(after.iter().take(SNIPPET_CONTEXT));
    if after.len() > SNIPPET_CONTEXT {
        out.push('…');
    }
    out.chars().map(|c| if c == '\n' || c == '\t' || c == '\r' { ' ' } else { c }).collect()
}

/// Search the document.
pub fn find(doc: &WordDoc, anchors: &Anchors, sheet: &StyleSheet, req: &FindRequest) -> Result<FindResult> {
    find_with(doc, anchors, sheet, None, req)
}

pub(crate) fn find_with(
    doc: &WordDoc,
    anchors: &Anchors,
    sheet: &StyleSheet,
    idx: Option<&DocIndex>,
    req: &FindRequest,
) -> Result<FindResult> {
    let re = build_regex(req)?;
    if let Some(s) = &req.story
        && anchors.story_index(s).is_none()
    {
        return Err(Error::Anchor(format!("unknown story {s:?}")));
    }
    let limit = req.limit.clamp(1, MAX_FIND_LIMIT);
    // Deleted text only exists in the markup view's text model.
    let cx = DocCtx::with(doc, anchors, sheet, &req.options, View::Markup, idx);
    let markup = req.view == View::Markup;
    let mut hits = Vec::new();
    let mut total = 0usize;
    for si in 0..doc.stories().len() {
        let sid = cx.story_id(si);
        if req.story.as_deref().is_some_and(|s| s != sid) {
            continue;
        }
        let mut st = InlineState::default();
        for (h, loc) in anchors.handles(si) {
            let pieces = inline::pieces(&cx, loc.part, loc.node, &mut st);
            let (text, del) = para_text(&cx, &pieces, markup);
            if text.is_empty() {
                continue;
            }
            for m in re.find_iter(&text) {
                if m.start() == m.end() {
                    continue;
                }
                total += 1;
                if hits.len() >= limit {
                    continue;
                }
                hits.push(FindHit {
                    anchor: h.to_owned(),
                    story: sid.clone(),
                    start: utf16_at(&text, m.start()),
                    end: utf16_at(&text, m.end()),
                    text: m.as_str().to_owned(),
                    snippet: snippet(&text, m.start(), m.end()),
                    deleted: del.iter().any(|&(b, e)| b < m.end() && m.start() < e),
                });
            }
        }
    }
    let mut seen = BTreeSet::new();
    let handles: Vec<&str> = hits.iter().map(|h| h.anchor.as_str()).filter(|a| seen.insert(*a)).collect();
    let lines = render::block_lines(doc, anchors, sheet, Some(&*cx), &handles, req.view, &req.options)?;
    Ok(FindResult { hits, total, lines })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_cut_and_flatten() {
        let s = format!("{}needle\tx", "a".repeat(50));
        let b = 50;
        let sn = snippet(&s, b, b + 6);
        assert!(sn.starts_with('…'));
        assert!(sn.ends_with("needle x"));
    }

    #[test]
    fn regex_options() {
        let mut r = FindRequest { query: "a.b".into(), ..Default::default() };
        assert!(build_regex(&r).unwrap().is_match("A.B"));
        assert!(!build_regex(&r).unwrap().is_match("axb"));
        r.regex = true;
        assert!(build_regex(&r).unwrap().is_match("axb"));
        r.whole_word = true;
        assert!(!build_regex(&r).unwrap().is_match("zaxb"));
        r.case_sensitive = true;
        assert!(!build_regex(&r).unwrap().is_match("AXB"));
        assert!(build_regex(&FindRequest::default()).is_err());
    }
}
