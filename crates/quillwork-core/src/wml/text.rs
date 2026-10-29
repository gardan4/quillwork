//! Text of paragraphs, runs and other inline containers, in three views of tracked changes.
//!
//! Rules (shared by every caller, so text from different entry points always agrees):
//! - `w:t`, `m:t` give their text; `w:delText` gives its text except in [`TextMode::AcceptAll`].
//! - `w:tab`/`w:ptab` → `\t`; `w:br`/`w:cr` → `\n`; `w:noBreakHyphen` → U+2011;
//!   `w:softHyphen` → U+00AD; `w:sym` → the character in `@w:char`.
//! - Accept-all skips `w:del`/`w:moveFrom` subtrees; reject-all skips `w:ins`/`w:moveTo`.
//! - Paragraphs are joined with `\n`, except after a paragraph whose mark is deleted (accept-all)
//!   or inserted (reject-all): that paragraph runs on into the next, as in
//!   [`paragraph_texts`](super::view::paragraph_texts).
//! - Never descended: property elements, field codes (`w:instrText`, `w:delInstrText`), text
//!   boxes (their own stories), drawings, ignored `mc:AlternateContent` branches.

use super::tree::{Step, is_ignored_branch, is_props, w_attr, walk};
use super::view::{Paragraph, RevisionKind};
use crate::xml::{Document, NodeId, ns};

/// Which view of tracked changes text is read in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextMode {
    /// Everything as marked up: inserted and deleted text both present.
    #[default]
    AsIs,
    /// As if every tracked change were accepted.
    AcceptAll,
    /// As if every tracked change were rejected.
    RejectAll,
}

/// A stretch of text that shares one revision state (as-is view).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    /// Innermost enclosing `w:ins`/`w:del`/`w:moveFrom`/`w:moveTo` (within the container), or
    /// `None` for untracked text.
    pub revision: Option<NodeId>,
}

/// Text of `n`'s content (a paragraph, run, hyperlink, cell, … or a whole story root).
/// Paragraphs met on the way are joined with `\n`.
pub fn text(doc: &Document, n: NodeId, mode: TextMode) -> String {
    let mut out = String::new();
    visit(doc, n, mode, |_, s| out.push_str(s));
    out
}

/// As-is text of `n` split by revision state.
pub fn segments(doc: &Document, n: NodeId) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    visit(doc, n, TextMode::AsIs, |at, s| {
        let rev = doc
            .ancestors(at)
            .take_while(|&a| a != n)
            .find(|&a| matches!(rev_local(doc, a), Some("ins" | "del" | "moveFrom" | "moveTo")));
        match out.last_mut() {
            Some(last) if last.revision == rev => last.text.push_str(s),
            _ => out.push(Segment { text: s.to_owned(), revision: rev }),
        }
    });
    out
}

fn rev_local(doc: &Document, n: NodeId) -> Option<&str> {
    if doc.namespace(n) == Some(ns::W) { doc.local_name(n) } else { None }
}

/// Core walker: calls `emit(element, text)` for every piece of text below `n` in `mode`.
pub(crate) fn visit(doc: &Document, n: NodeId, mode: TextMode, mut emit: impl FnMut(NodeId, &str)) {
    let mut paras = 0usize;
    // The last paragraph's mark goes away in `mode`, so the next paragraph continues it.
    let mut runs_on = false;
    // `n` itself may be a text element (a lone `w:t`).
    if leaf(doc, n, mode, &mut emit) {
        return;
    }
    walk(doc, n, |c| {
        if is_ignored_branch(doc, c) {
            return Step::Over;
        }
        let w = doc.namespace(c) == Some(ns::W);
        let local = doc.local_name(c).unwrap_or("");
        if w {
            match local {
                "ins" | "moveTo" if mode == TextMode::RejectAll => return Step::Over,
                "del" | "moveFrom" if mode == TextMode::AcceptAll => return Step::Over,
                "instrText" | "delInstrText" | "txbxContent" | "drawing" | "pict" | "object" => {
                    return Step::Over;
                }
                "p" => {
                    if paras > 0 && !runs_on {
                        emit(c, "\n");
                    }
                    paras += 1;
                    runs_on = matches!(
                        (mode, Paragraph(c).mark_revision(doc).map(|r| r.kind)),
                        (TextMode::AcceptAll, Some(RevisionKind::ParaMarkDel))
                            | (TextMode::RejectAll, Some(RevisionKind::ParaMarkIns))
                    );
                    return Step::Into;
                }
                l if is_props(l) => return Step::Over,
                _ => {}
            }
        }
        if leaf(doc, c, mode, &mut emit) { Step::Over } else { Step::Into }
    });
}

/// Emit the text of a text-producing element. Returns whether `c` was one.
fn leaf(doc: &Document, c: NodeId, mode: TextMode, emit: &mut impl FnMut(NodeId, &str)) -> bool {
    match doc.namespace(c) {
        Some(ns::W) => {}
        Some(ns::M) if doc.local_name(c) == Some("t") => {
            emit(c, &doc.text_content(c));
            return true;
        }
        _ => return false,
    }
    match doc.local_name(c).unwrap_or("") {
        "t" => emit(c, &doc.text_content(c)),
        "delText" => {
            if mode != TextMode::AcceptAll {
                emit(c, &doc.text_content(c));
            }
        }
        "tab" | "ptab" => emit(c, "\t"),
        "br" | "cr" => emit(c, "\n"),
        "noBreakHyphen" => emit(c, "\u{2011}"),
        "softHyphen" => emit(c, "\u{00AD}"),
        "sym" => {
            if let Some(ch) =
                w_attr(doc, c, "char").and_then(|v| u32::from_str_radix(v.trim(), 16).ok()).and_then(char::from_u32)
            {
                let mut buf = [0u8; 4];
                emit(c, ch.encode_utf8(&mut buf));
            }
        }
        _ => return false,
    }
    true
}

/// 64-bit FNV-1a of a string, the text fingerprint used by anchors.
pub fn fingerprint(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
