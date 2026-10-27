//! Small tree-walking helpers shared by the Word model views.

use crate::xml::{Document, NodeId, ns};

/// What a walker callback wants after seeing an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Visit the element's children next.
    Into,
    /// Skip the element's subtree.
    Over,
}

/// Pre-order walk over the elements below `root` (not `root` itself). `f` decides per element
/// whether to descend. Iterative, so deep trees cannot overflow the stack.
pub(crate) fn walk(doc: &Document, root: NodeId, mut f: impl FnMut(NodeId) -> Step) {
    let mut cur = doc.first_child(root);
    let mut stack: Vec<Option<NodeId>> = Vec::new();
    loop {
        let Some(n) = cur else {
            match stack.pop() {
                Some(resume) => {
                    cur = resume;
                    continue;
                }
                None => break,
            }
        };
        let next = doc.next_sibling(n);
        if doc.is_element_node(n)
            && f(n) == Step::Into
            && let Some(c) = doc.first_child(n)
        {
            stack.push(next);
            cur = Some(c);
            continue;
        }
        cur = next;
    }
}

/// `w:<local>`?
#[inline]
pub(crate) fn is_w(doc: &Document, n: NodeId, local: &str) -> bool {
    doc.is(n, ns::W, local)
}

/// Local name of an element in the `w:` namespace, else `None`.
#[inline]
pub(crate) fn w_local(doc: &Document, n: NodeId) -> Option<&str> {
    if doc.namespace(n) == Some(ns::W) { doc.local_name(n) } else { None }
}

/// `w:<local>/@w:val`-style lookup: the `w:`-namespaced attribute `local` of `n`.
#[inline]
pub(crate) fn w_attr(doc: &Document, n: NodeId, local: &str) -> Option<String> {
    doc.attr_ns(n, ns::W, local).map(|c| c.into_owned())
}

/// `@w:val` of the child `w:<child>` of `n`.
pub(crate) fn child_val(doc: &Document, n: NodeId, child: &str) -> Option<String> {
    doc.find_child(n, ns::W, child).and_then(|c| w_attr(doc, c, "val"))
}

/// A child `w:<child>` that is an on/off toggle: present and not `val="0|false|off"`.
pub(crate) fn child_on(doc: &Document, n: NodeId, child: &str) -> bool {
    match doc.find_child(n, ns::W, child) {
        None => false,
        Some(c) => !matches!(w_attr(doc, c, "val").as_deref(), Some("0" | "false" | "off")),
    }
}

/// Is `n` an `mc:AlternateContent` branch that readers must ignore: a `mc:Fallback` whose
/// `mc:AlternateContent` has a `mc:Choice`, or any `mc:Choice` after the first. The first
/// `mc:Choice` is the canonical copy.
pub(crate) fn is_ignored_branch(doc: &Document, n: NodeId) -> bool {
    let fallback = doc.is(n, ns::MC, "Fallback");
    let choice = doc.is(n, ns::MC, "Choice");
    if !fallback && !choice {
        return false;
    }
    let Some(ac) = doc.parent(n) else { return false };
    let first_choice = doc.find_child(ac, ns::MC, "Choice");
    if fallback { first_choice.is_some() } else { first_choice != Some(n) }
}

/// Property containers whose subtrees never hold content.
pub(crate) fn is_props(local: &str) -> bool {
    matches!(
        local,
        "pPr"
            | "rPr"
            | "sdtPr"
            | "sdtEndPr"
            | "sectPr"
            | "tblPr"
            | "trPr"
            | "tcPr"
            | "tblGrid"
            | "tblPrEx"
            | "customXmlPr"
            | "smartTagPr"
            | "fldData"
    )
}

/// Parse an 8-hex-digit `w14:paraId` / `w14:textId`.
pub(crate) fn parse_hex_id(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 8 {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

/// Parse a decimal attribute, tolerating surrounding whitespace.
pub(crate) fn parse_int(s: &str) -> Option<i64> {
    s.trim().parse().ok()
}
