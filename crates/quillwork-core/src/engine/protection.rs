//! Mutation policy for declared Word restrictions. These are editing safeguards, not
//! encryption or an authorization system; permission-range identities are not resolved.

// The operation error is the shared public envelope, constructed only on refusal.
#![allow(clippy::result_large_err)]

use crate::edit::Mode;
use crate::ops::OpError;
use crate::wml::{PartIdx, PartKind, WordDoc};
use crate::xml::{Document, NodeId, ns};

fn refusal(reason: &str, message: &str) -> OpError {
    OpError { reason: Some(reason.into()), ..OpError::new("PROTECTED", message) }
}

pub(super) fn comment_op(name: &str) -> bool {
    matches!(name, "add_comment" | "reply_comment" | "edit_comment" | "resolve_comment" | "delete_comment")
}

fn on(value: &str) -> bool {
    matches!(value, "1" | "true" | "on")
}

/// Preserve explicitly allowed comment and tracked-edit categories. Forms, permission
/// exceptions and restricted style sets require host identities/style policy we do not have.
pub(super) fn document(doc: &WordDoc, name: &str, mode: Mode) -> Result<(), OpError> {
    let Some(settings) = doc.settings_part() else { return Ok(()) };
    let d = doc.part(settings);
    for protection in d.child_elements(d.root()).filter(|&n| d.is(n, ns::W, "documentProtection")) {
        if !d.attr_ns(protection, ns::W, "enforcement").is_some_and(|v| on(&v)) {
            continue;
        }
        let comments = comment_op(name);
        if d.attr_ns(protection, ns::W, "formatting").is_some_and(|v| on(&v)) && !comments {
            return Err(refusal("document_protection", "restricted formatting requires a host-resolved style policy"));
        }
        let edit = d.attr_ns(protection, ns::W, "edit");
        let allowed = match edit.as_deref().unwrap_or("none") {
            "none" => true,
            "comments" => comments,
            "trackedChanges" => {
                comments || (mode == Mode::Track && !matches!(name, "accept" | "reject" | "set_track_changes"))
            }
            _ => false,
        };
        if !allowed {
            return Err(refusal(
                "document_protection",
                "the document's enforced editing restriction forbids this operation",
            ));
        }
    }
    Ok(())
}

struct Locked {
    part: PartIdx,
    node: NodeId,
    content: Option<(NodeId, Vec<u8>)>,
    deleted: bool,
}

/// Capture actual locked subtrees before each operation. Comparing the result covers table
/// ancestors/descendants and review selectors without maintaining a second target resolver.
pub(super) struct Controls(Vec<Locked>);

impl Controls {
    pub fn capture(doc: &WordDoc, comments: bool) -> Self {
        let mut locked = Vec::new();
        for part in doc.part_indices() {
            // Annotation markers may be added to a locked story without changing its
            // contents. A locked SDT inside a comment still protects that comment text.
            if comments && doc.part_kind(part) != PartKind::Comments {
                continue;
            }
            let d = doc.part(part);
            for node in d.descendants_named(d.root(), ns::W, "sdt") {
                let value = d
                    .find_child(node, ns::W, "sdtPr")
                    .and_then(|n| d.find_child(n, ns::W, "lock"))
                    .and_then(|n| d.attr_ns(n, ns::W, "val"));
                let Some(value) = value else { continue };
                if !matches!(&*value, "contentLocked" | "sdtContentLocked" | "sdtLocked") {
                    continue;
                }
                let content = if &*value == "sdtLocked" {
                    None
                } else {
                    let content = d.find_child(node, ns::W, "sdtContent").unwrap_or(node);
                    Some((content, d.node_to_bytes(content)))
                };
                locked.push(Locked { part, node, content, deleted: deleted(d, node) });
            }
        }
        Self(locked)
    }

    pub fn check(self, doc: &WordDoc) -> Result<(), OpError> {
        for locked in self.0 {
            let d = doc.part(locked.part);
            let attached = std::iter::once(locked.node).chain(d.ancestors(locked.node)).any(|n| n == d.root());
            if !attached
                || deleted(d, locked.node) != locked.deleted
                || locked.content.is_some_and(|(node, before)| d.node_to_bytes(node) != before)
            {
                return Err(refusal(
                    "locked_control",
                    "the operation would change locked content or remove a locked control",
                ));
            }
        }
        Ok(())
    }
}

fn deleted(d: &Document, node: NodeId) -> bool {
    std::iter::once(node).chain(d.ancestors(node)).any(|n| {
        d.is(n, ns::W, "del")
            || d.is(n, ns::W, "moveFrom")
            || (d.is(n, ns::W, "tr")
                && d.find_child(n, ns::W, "trPr").is_some_and(|pr| d.find_child(pr, ns::W, "del").is_some()))
            || (d.is(n, ns::W, "tc")
                && d.find_child(n, ns::W, "tcPr").is_some_and(|pr| d.find_child(pr, ns::W, "cellDel").is_some()))
    })
}
