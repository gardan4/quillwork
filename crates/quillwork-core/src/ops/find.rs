//! Locating text inside target paragraphs.
//!
//! Matching tries, in order: the needle as given; a folded match (non-breaking and figure spaces
//! read as spaces, curly quotes as straight ones, whitespace runs collapsed); and the folded
//! needle with markdown `**` stripped. Offsets always refer to the paragraph's accepted-mode text.

use super::{Candidate, OpError, OpResult};
use crate::wml::text::{TextMode, text};
use crate::xml::{Document, NodeId};

/// One match: byte range in the accepted text of `paras[para]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub para: usize,
    pub start: usize,
    pub end: usize,
}

fn fold_char(c: char) -> char {
    match c {
        '\u{00A0}' | '\u{202F}' | '\u{2007}' | '\u{2009}' | '\u{200A}' => ' ',
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' => '"',
        '\u{2013}' | '\u{2014}' | '\u{2011}' | '\u{2010}' => '-',
        c => c,
    }
}

/// Folded text plus, per folded byte boundary, the original byte offset (`starts`) and the
/// original end of the char that ends there (`ends`).
struct Folded {
    text: String,
    starts: Vec<usize>,
    ends: Vec<usize>,
}

fn fold(s: &str) -> Folded {
    let mut text = String::with_capacity(s.len());
    let mut starts = Vec::with_capacity(s.len() + 1);
    let mut ends = Vec::with_capacity(s.len() + 1);
    ends.push(0);
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let f = fold_char(c);
        if f.is_whitespace() && f != '\n' && f != '\t' {
            // A run of plain whitespace collapses to one space.
            let mut end = i + c.len_utf8();
            while let Some(&(j, d)) = chars.peek() {
                let g = fold_char(d);
                if g.is_whitespace() && g != '\n' && g != '\t' {
                    end = j + d.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            starts.push(i);
            text.push(' ');
            ends.push(end);
        } else {
            let mut buf = [0u8; 4];
            let enc = f.encode_utf8(&mut buf);
            for k in 0..enc.len() {
                starts.push(i);
                if k + 1 < enc.len() {
                    ends.push(i);
                }
            }
            text.push(f);
            ends.push(i + c.len_utf8());
        }
    }
    starts.push(s.len());
    Folded { text, starts, ends }
}

fn fold_needle(s: &str) -> String {
    fold(s).text
}

/// Non-overlapping byte ranges of `needle` in `hay`.
fn exact(hay: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = hay[from..].find(needle) {
        let s = from + i;
        out.push((s, s + needle.len()));
        from = s + needle.len();
    }
    out
}

fn folded_hits(hay: &str, needle: &str) -> Vec<(usize, usize)> {
    let f = fold(hay);
    let n = fold_needle(needle);
    exact(&f.text, &n).into_iter().map(|(s, e)| (f.starts[s], f.ends[e])).collect()
}

/// All matches of `needle` across `texts` (one per paragraph), numbered in order.
pub fn find_all(texts: &[String], needle: &str) -> Vec<Hit> {
    let pass = |f: &dyn Fn(&str) -> Vec<(usize, usize)>| -> Vec<Hit> {
        let mut out = Vec::new();
        for (i, t) in texts.iter().enumerate() {
            for (s, e) in f(t) {
                out.push(Hit { para: i, start: s, end: e });
            }
        }
        out
    };
    let hits = pass(&|t| exact(t, needle));
    if !hits.is_empty() {
        return hits;
    }
    let hits = pass(&|t| folded_hits(t, needle));
    if !hits.is_empty() {
        return hits;
    }
    let stripped = needle.replace("**", "");
    if stripped != needle && !stripped.trim().is_empty() {
        return pass(&|t| folded_hits(t, &stripped));
    }
    Vec::new()
}

/// Accepted-mode text of each paragraph.
pub fn texts(d: &Document, paras: &[NodeId]) -> Vec<String> {
    paras.iter().map(|&p| text(d, p, TextMode::AcceptAll)).collect()
}

/// `±30` characters of context around a hit.
pub fn context(t: &str, start: usize, end: usize) -> String {
    let before: String = {
        let chars: Vec<char> = t[..start].chars().collect();
        let k = chars.len().saturating_sub(30);
        let s: String = chars[k..].iter().collect();
        if k > 0 { format!("…{s}") } else { s }
    };
    let after: String = {
        let chars: Vec<char> = t[end..].chars().collect();
        let s: String = chars.iter().take(30).collect();
        if chars.len() > 30 { format!("{s}…") } else { s }
    };
    format!("{before}{}{after}", &t[start..end])
}

/// Pick one hit: `occurrence` (1-based) or the only one. `handle_of(i)` names paragraph `i`.
pub fn pick(
    hits: &[Hit],
    texts: &[String],
    needle: &str,
    occurrence: Option<u32>,
    target: &str,
    path: String,
    handle_of: &dyn Fn(usize) -> String,
) -> OpResult<Hit> {
    if hits.is_empty() {
        let mut e = OpError::not_found("text", format!("{needle:?} not found in {target}"));
        e.anchor = Some(target.to_owned());
        e.wants_current = true;
        return Err(e);
    }
    match occurrence {
        Some(n) => {
            let n = n as usize;
            if n == 0 || n > hits.len() {
                return Err(OpError::invalid(
                    path,
                    format!("occurrence {n} of {needle:?}: {target} has {} occurrence(s)", hits.len()),
                ));
            }
            Ok(hits[n - 1])
        }
        None if hits.len() == 1 => Ok(hits[0]),
        None => {
            let candidates = hits
                .iter()
                .enumerate()
                .take(20)
                .map(|(i, h)| Candidate {
                    anchor: handle_of(h.para),
                    occurrence: Some(i as u32 + 1),
                    context: context(&texts[h.para], h.start, h.end),
                })
                .collect();
            Err(OpError {
                anchor: Some(target.to_owned()),
                candidates: Some(candidates),
                ..OpError::new(
                    "AMBIGUOUS",
                    format!("{needle:?} occurs {} times in {target}; give occurrence", hits.len()),
                )
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_maps_back_to_original_offsets() {
        let t = "40\u{00A0}uur  per \u{2018}week\u{2019}".to_owned();
        let hits = find_all(std::slice::from_ref(&t), "40 uur per 'week'");
        assert_eq!(hits.len(), 1);
        assert_eq!(&t[hits[0].start..hits[0].end], t.as_str());
        let hits = find_all(std::slice::from_ref(&t), "uur per");
        assert_eq!(&t[hits[0].start..hits[0].end], "uur  per");
    }

    #[test]
    fn exact_before_folded_and_markdown_stripped() {
        let t = vec!["de **vaste** dag".replace("**", ""), "x".to_owned()];
        let hits = find_all(&t, "**vaste**");
        assert_eq!(hits, vec![Hit { para: 0, start: 3, end: 8 }]);
        let hits = find_all(&["a a a".to_owned()], "a");
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn ambiguous_lists_candidates() {
        let t = vec!["de werknemer en de werkgever".to_owned()];
        let hits = find_all(&t, "de");
        let e = pick(&hits, &t, "de", None, "b1", "/ops/0/find".into(), &|_| "b1".into()).unwrap_err();
        assert_eq!(e.code, "AMBIGUOUS");
        assert_eq!(e.candidates.unwrap().len(), 2);
        let h = pick(&hits, &t, "de", Some(2), "b1", "/ops/0/find".into(), &|_| "b1".into()).unwrap();
        assert_eq!(h.start, 16);
    }
}
