//! Revision ids. One allocator per document, shared by every part and every writer (tracked
//! changes, comments, bookmarks), so a new `w:id` never collides with one already in the package.
//! Ids stay below `0x80000000`.

use std::collections::HashSet;

use crate::wml::WordDoc;
use crate::xml::ns;

const MAX_ID: u32 = 0x7FFF_FFFF;

/// Hands out fresh `w:id` values.
#[derive(Debug, Clone, Default)]
pub struct IdAlloc {
    used: HashSet<u32>,
    next: u32,
    /// Every id handed out, in order.
    issued: Vec<u32>,
}

impl IdAlloc {
    /// Scan every loaded part for `w:id` values on WordprocessingML elements.
    pub fn scan(doc: &WordDoc) -> IdAlloc {
        let mut used = HashSet::new();
        for idx in doc.part_indices() {
            let d = doc.part(idx);
            for n in d.descendants(d.root()) {
                if !d.is_element_node(n) || d.attr_count(n) == 0 || d.namespace(n) != Some(ns::W) {
                    continue;
                }
                let id = d.attr_ns(n, ns::W, "id").and_then(|v| v.trim().parse::<i64>().ok());
                if let Some(id) = id.filter(|id| (0..=i64::from(MAX_ID)).contains(id)) {
                    used.insert(id as u32);
                }
            }
        }
        let next = used.iter().copied().filter(|&i| i < MAX_ID).max().map_or(1, |m| m + 1);
        IdAlloc { used, next, issued: Vec::new() }
    }

    /// Mark ids as taken (ids issued by an earlier session, for example).
    pub fn reserve(&mut self, ids: impl IntoIterator<Item = u32>) {
        for id in ids {
            self.used.insert(id);
            if id >= self.next && id < MAX_ID {
                self.next = id + 1;
            }
        }
    }

    /// A fresh id: above every id seen, or the lowest free one once that range is exhausted.
    pub fn alloc(&mut self) -> u32 {
        if self.next == 0 || self.next > MAX_ID {
            self.next = 1;
        }
        while self.used.contains(&self.next) {
            self.next = if self.next >= MAX_ID { 1 } else { self.next + 1 };
        }
        let id = self.next;
        self.used.insert(id);
        self.issued.push(id);
        self.next = id.saturating_add(1);
        id
    }

    /// Ids handed out so far.
    pub fn issued(&self) -> &[u32] {
        &self.issued
    }

    /// Forget ids handed out after the first `len` (transaction rollback).
    pub fn truncate(&mut self, len: usize) {
        for id in self.issued.drain(len..) {
            self.used.remove(&id);
            if id < self.next {
                self.next = id;
            }
        }
    }
}
