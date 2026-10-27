//! Mutations. Every one marks exactly what it touched as dirty, so serialisation keeps the
//! rest of the part byte-identical.
//!
//! Insertion functions *move* a node that is already attached somewhere (detach + insert).

use super::escape::{self, xmlns_prefix};
use super::parse::resolve;
use super::{
    Attrs, CONTENT_DIRTY, Data, Document, Elem, NAME_DIRTY, NIL, NS_NONE, NS_UNBOUND, Node, NodeId, NodeKind, OwnAttr,
    Raw, TAG_DIRTY,
};
use crate::{Error, Result};

fn node_err(msg: impl Into<String>) -> Error {
    Error::XmlNode(msg.into())
}

impl Document {
    fn check(&self, n: NodeId) -> Result<()> {
        if self.valid(n) { Ok(()) } else { Err(node_err(format!("no node with id {}", n.0))) }
    }

    fn check_element(&self, n: NodeId) -> Result<()> {
        self.check(n)?;
        if self.is_element_node(n) { Ok(()) } else { Err(node_err("not an element")) }
    }

    /// Mark `p` and its ancestors content-dirty. Invariant: a content-dirty attached node has
    /// content-dirty ancestors, so we can stop at the first one already marked.
    fn mark_dirty(&mut self, mut p: u32) {
        while p != NIL {
            let node = &mut self.nodes[p as usize];
            if node.flags & CONTENT_DIRTY != 0 {
                break;
            }
            node.flags |= CONTENT_DIRTY;
            p = node.parent;
        }
    }

    fn push_node(&mut self, data: Data) -> Result<NodeId> {
        if self.nodes.len() >= self.limits.max_nodes.min(NIL as usize - 1) {
            return Err(Error::Limit(format!("more than {} xml nodes", self.limits.max_nodes)));
        }
        self.nodes.push(Node::new(data));
        Ok(NodeId(self.nodes.len() as u32 - 1))
    }

    fn new_string(&mut self, s: String) -> Raw {
        self.strings.push(s);
        Raw::Own(self.strings.len() as u32 - 1)
    }

    /// Replace the string behind `r` (in place if it is already ours; owned strings are never
    /// shared between nodes).
    fn set_string(&mut self, r: Raw, s: String) -> Raw {
        match r {
            Raw::Own(i) => {
                self.strings[i as usize] = s;
                r
            }
            Raw::Src(..) => self.new_string(s),
        }
    }

    /// Switch an element's attribute list to the editable form; returns its `own_attrs` index.
    fn own_attrs_mut(&mut self, n: NodeId) -> u32 {
        let Data::Element(e) = self.nodes[n.0 as usize].data else { unreachable!("checked element") };
        match e.attrs {
            Attrs::Own(i) => i,
            Attrs::Src { first, len } => {
                let list: Vec<OwnAttr> = (first..first + len)
                    .map(|k| {
                        let a = self.src_attrs[k as usize];
                        OwnAttr {
                            name: Raw::Src(a.name, a.name_end),
                            value: Raw::Src(a.val, a.val_end),
                            quote: self.src.as_bytes()[a.val as usize - 1],
                            src: k,
                            changed: false,
                        }
                    })
                    .collect();
                self.own_attrs.push(list);
                let idx = self.own_attrs.len() as u32 - 1;
                if let Data::Element(e) = &mut self.nodes[n.0 as usize].data {
                    e.attrs = Attrs::Own(idx);
                }
                idx
            }
        }
    }

    fn touch_tag(&mut self, n: NodeId) {
        self.nodes[n.0 as usize].flags |= TAG_DIRTY;
        let p = self.nodes[n.0 as usize].parent;
        self.mark_dirty(p);
    }

    // ------------------------------------------------------------------ attributes

    /// Set an attribute by qualified name, adding it at the end if absent. An existing
    /// attribute keeps its position, leading whitespace and quote character. Setting the value
    /// it already has changes nothing. Setting `xmlns` / `xmlns:p` re-resolves the subtree.
    pub fn set_attr(&mut self, n: NodeId, qname: &str, value: &str) -> Result<()> {
        self.check_element(n)?;
        escape::check_qname(qname)?;
        let decl = xmlns_prefix(qname);
        if let Some(p) = decl {
            if p == "xml" || p == "xmlns" {
                return Err(Error::XmlValue(format!("cannot declare reserved prefix {p:?}")));
            }
            if value.is_empty() && !p.is_empty() {
                return Err(Error::XmlValue("a prefix cannot be undeclared".into()));
            }
        }
        if self.attr(n, qname).as_deref() == Some(value) {
            return Ok(());
        }
        let idx = self.own_attrs_mut(n) as usize;
        let pos = (0..self.own_attrs[idx].len()).find(|&i| self.raw(self.own_attrs[idx][i].name) == qname);
        match pos {
            Some(i) => {
                let a = self.own_attrs[idx][i];
                let escaped = escape::escape_attr(value, a.quote)?;
                let v = self.set_string(a.value, escaped);
                let a = &mut self.own_attrs[idx][i];
                a.value = v;
                a.changed = true;
            }
            None => {
                let escaped = escape::escape_attr(value, b'"')?;
                if self.own_attrs[idx].len() >= self.limits.max_attrs {
                    return Err(Error::Limit(format!("more than {} attributes", self.limits.max_attrs)));
                }
                let name = self.new_string(qname.to_owned());
                let value = self.new_string(escaped);
                self.own_attrs[idx].push(OwnAttr { name, value, quote: b'"', src: NIL, changed: true });
            }
        }
        self.touch_tag(n);
        if decl.is_some() {
            self.recompute_ns(n.0);
        }
        Ok(())
    }

    /// Set an attribute by namespace URI and local name. An existing match is updated;
    /// otherwise a prefix bound to `ns` in scope is used (`Error::XmlNode` if there is none —
    /// declare one first with `set_attr(root, "xmlns:p", uri)`). `ns = ""` means unprefixed.
    pub fn set_attr_ns(&mut self, n: NodeId, ns: &str, local: &str, value: &str) -> Result<()> {
        self.check_element(n)?;
        for i in 0..self.attr_count(n) {
            let Some((k, _)) = self.attr_raw_at(n, i) else { break };
            let (p, l) = escape::split_qname(k);
            if l != local || xmlns_prefix(k).is_some() {
                continue;
            }
            let hit = if p.is_empty() { ns.is_empty() } else { self.lookup_namespace(n, p) == Some(ns) };
            if hit {
                let k = k.to_owned();
                return self.set_attr(n, &k, value);
            }
        }
        let qname = if ns.is_empty() {
            local.to_owned()
        } else {
            match self.find_prefix(n, ns) {
                Some(p) if !p.is_empty() => format!("{p}:{local}"),
                _ => return Err(node_err(format!("no prefix bound to {ns} here"))),
            }
        };
        self.set_attr(n, &qname, value)
    }

    /// Remove an attribute by qualified name. Returns whether it was there.
    pub fn remove_attr(&mut self, n: NodeId, qname: &str) -> Result<bool> {
        self.check_element(n)?;
        if self.attr(n, qname).is_none() {
            return Ok(false);
        }
        let idx = self.own_attrs_mut(n) as usize;
        let pos = (0..self.own_attrs[idx].len()).find(|&i| self.raw(self.own_attrs[idx][i].name) == qname);
        if let Some(i) = pos {
            self.own_attrs[idx].remove(i);
        }
        self.touch_tag(n);
        if xmlns_prefix(qname).is_some() {
            self.recompute_ns(n.0);
        }
        Ok(true)
    }

    /// Rename an element (e.g. `w:t` → `w:delText`). Both tags are regenerated.
    pub fn set_name(&mut self, n: NodeId, qname: &str) -> Result<()> {
        self.check_element(n)?;
        escape::check_qname(qname)?;
        if self.name(n) == Some(qname) {
            return Ok(());
        }
        let Data::Element(e) = self.nodes[n.0 as usize].data else { unreachable!() };
        let name = self.set_string(e.name, qname.to_owned());
        if let Data::Element(e) = &mut self.nodes[n.0 as usize].data {
            e.name = name;
        }
        self.nodes[n.0 as usize].flags |= NAME_DIRTY;
        self.touch_tag(n);
        self.resolve_own_ns(n);
        Ok(())
    }

    // ------------------------------------------------------------------ text

    /// Set the value of a text node, CDATA section or comment. On an element, replace all its
    /// children with one text node (reusing the existing text node if it is the only child).
    /// Setting the value a node already has changes nothing.
    pub fn set_text(&mut self, n: NodeId, text: &str) -> Result<()> {
        self.check(n)?;
        match self.nodes[n.0 as usize].data {
            Data::Leaf(kind, raw) => {
                if self.text(n).as_deref() == Some(text) {
                    return Ok(());
                }
                let new = match kind {
                    NodeKind::Text => escape::escape_text(text)?,
                    NodeKind::CData => {
                        escape::check_cdata(text)?;
                        format!("<![CDATA[{text}]]>")
                    }
                    NodeKind::Comment => {
                        escape::check_comment(text)?;
                        format!("<!--{text}-->")
                    }
                    _ => return Err(node_err("set_text: not a text, CDATA or comment node")),
                };
                if kind == NodeKind::Text && self.nodes[n.0 as usize].parent == 0 && !text.trim_ascii().is_empty() {
                    return Err(node_err("only whitespace text may sit outside the root element"));
                }
                let r = self.set_string(raw, new);
                self.nodes[n.0 as usize].data = Data::Leaf(kind, r);
                let p = self.nodes[n.0 as usize].parent;
                self.mark_dirty(p);
                Ok(())
            }
            Data::Element(_) => {
                let first = self.nodes[n.0 as usize].first;
                let only_text = first != NIL
                    && self.nodes[first as usize].next == NIL
                    && matches!(self.nodes[first as usize].data, Data::Leaf(NodeKind::Text, _));
                if only_text {
                    return self.set_text(NodeId(first), text);
                }
                let escaped = escape::escape_text(text)?;
                while let Some(c) = self.first_child(n) {
                    self.unlink(c.0);
                }
                if !text.is_empty() {
                    let raw = self.new_string(escaped);
                    let t = self.push_node(Data::Leaf(NodeKind::Text, raw))?;
                    self.link_child(t.0, n.0, NIL);
                }
                self.mark_dirty(n.0);
                Ok(())
            }
            Data::Document => Err(node_err("set_text on the document node")),
        }
    }

    // ------------------------------------------------------------------ creation

    /// Create a detached element. Its namespace is resolved when it is inserted.
    pub fn create_element(&mut self, qname: &str) -> Result<NodeId> {
        escape::check_qname(qname)?;
        let name = self.new_string(qname.to_owned());
        self.own_attrs.push(Vec::new());
        let attrs = Attrs::Own(self.own_attrs.len() as u32 - 1);
        let ns = if escape::split_qname(qname).0.is_empty() { NS_NONE } else { NS_UNBOUND };
        self.push_node(Data::Element(Elem { name, ns, start: (NIL, NIL), end: (NIL, NIL), attrs }))
    }

    /// Create a detached text node.
    pub fn create_text(&mut self, text: &str) -> Result<NodeId> {
        let raw = escape::escape_text(text)?;
        let r = self.new_string(raw);
        self.push_node(Data::Leaf(NodeKind::Text, r))
    }

    /// Create a detached comment.
    pub fn create_comment(&mut self, text: &str) -> Result<NodeId> {
        escape::check_comment(text)?;
        let r = self.new_string(format!("<!--{text}-->"));
        self.push_node(Data::Leaf(NodeKind::Comment, r))
    }

    /// Create a detached CDATA section.
    pub fn create_cdata(&mut self, text: &str) -> Result<NodeId> {
        escape::check_cdata(text)?;
        let r = self.new_string(format!("<![CDATA[{text}]]>"));
        self.push_node(Data::Leaf(NodeKind::CData, r))
    }

    /// Deep-copy a subtree. The copy is detached; insert it where you want it. Clean source
    /// nodes in the copy keep their source spans, so they still serialise as original bytes.
    pub fn clone_subtree(&mut self, n: NodeId) -> Result<NodeId> {
        self.check(n)?;
        if n.0 == 0 {
            return Err(node_err("cannot clone the document node"));
        }
        let new_root = self.clone_node(n.0)?;
        let mut stack = vec![(n.0, new_root)];
        while let Some((old, new)) = stack.pop() {
            let mut c = self.nodes[old as usize].first;
            while c != NIL {
                let nc = self.clone_node(c)?;
                self.link_child(nc, new, NIL);
                if self.nodes[c as usize].first != NIL {
                    stack.push((c, nc));
                }
                c = self.nodes[c as usize].next;
            }
        }
        Ok(NodeId(new_root))
    }

    fn clone_raw(&mut self, r: Raw) -> Raw {
        match r {
            Raw::Src(..) => r,
            Raw::Own(i) => {
                let s = self.strings[i as usize].clone();
                self.new_string(s)
            }
        }
    }

    fn clone_node(&mut self, n: u32) -> Result<u32> {
        let old = self.nodes[n as usize];
        let data = match old.data {
            Data::Document => Data::Document,
            Data::Leaf(k, r) => Data::Leaf(k, self.clone_raw(r)),
            Data::Element(mut e) => {
                e.name = self.clone_raw(e.name);
                if let Attrs::Own(i) = e.attrs {
                    let mut list = self.own_attrs[i as usize].clone();
                    for a in &mut list {
                        a.name = self.clone_raw(a.name);
                        a.value = self.clone_raw(a.value);
                    }
                    self.own_attrs.push(list);
                    e.attrs = Attrs::Own(self.own_attrs.len() as u32 - 1);
                }
                Data::Element(e)
            }
        };
        let id = self.push_node(data)?;
        self.nodes[id.0 as usize].flags = old.flags;
        Ok(id.0)
    }

    // ------------------------------------------------------------------ tree surgery

    fn unlink(&mut self, n: u32) {
        let Node { parent, prev, next, .. } = self.nodes[n as usize];
        if parent == NIL {
            return;
        }
        if prev != NIL {
            self.nodes[prev as usize].next = next;
        } else {
            self.nodes[parent as usize].first = next;
        }
        if next != NIL {
            self.nodes[next as usize].prev = prev;
        } else {
            self.nodes[parent as usize].last = prev;
        }
        let node = &mut self.nodes[n as usize];
        node.parent = NIL;
        node.prev = NIL;
        node.next = NIL;
        self.mark_dirty(parent);
    }

    /// Link detached `child` under `parent`, before `before` (or last when `before` is NIL).
    fn link_child(&mut self, child: u32, parent: u32, before: u32) {
        let prev = if before == NIL { self.nodes[parent as usize].last } else { self.nodes[before as usize].prev };
        {
            let c = &mut self.nodes[child as usize];
            c.parent = parent;
            c.prev = prev;
            c.next = before;
        }
        if prev != NIL {
            self.nodes[prev as usize].next = child;
        } else {
            self.nodes[parent as usize].first = child;
        }
        if before != NIL {
            self.nodes[before as usize].prev = child;
        } else {
            self.nodes[parent as usize].last = child;
        }
    }

    fn check_insert(&self, parent: NodeId, child: NodeId) -> Result<()> {
        self.check(parent)?;
        self.check(child)?;
        let ck = self.kind(child);
        if ck == Some(NodeKind::Document) {
            return Err(node_err("cannot insert the document node"));
        }
        if child.0 == self.root {
            return Err(node_err("cannot move the root element"));
        }
        match self.kind(parent) {
            Some(NodeKind::Element) => {}
            Some(NodeKind::Document) => {
                let ok = match ck {
                    Some(NodeKind::Comment | NodeKind::ProcessingInstruction) => true,
                    Some(NodeKind::Text) => self.text(child).is_some_and(|t| t.trim_ascii().is_empty()),
                    _ => false,
                };
                if !ok {
                    return Err(node_err("only comments, PIs and whitespace may sit outside the root"));
                }
            }
            _ => return Err(node_err("parent must be an element or the document")),
        }
        if parent == child || self.ancestors(parent).any(|a| a == child) {
            return Err(node_err("cannot insert a node inside itself"));
        }
        Ok(())
    }

    fn insert(&mut self, parent: NodeId, child: NodeId, before: impl Fn(&Self) -> u32) -> Result<()> {
        self.check_insert(parent, child)?;
        self.unlink(child.0);
        let before = before(self);
        self.link_child(child.0, parent.0, before);
        self.mark_dirty(parent.0);
        if self.is_element_node(child) {
            self.recompute_ns(child.0);
        }
        Ok(())
    }

    /// Append `child` as the last child of `parent` (moving it if attached elsewhere).
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> Result<()> {
        self.insert(parent, child, |_| NIL)
    }

    /// Insert `child` as the first child of `parent` (moving it if attached elsewhere).
    pub fn prepend_child(&mut self, parent: NodeId, child: NodeId) -> Result<()> {
        self.insert(parent, child, move |d| d.nodes[parent.0 as usize].first)
    }

    /// Insert `new` right before `reference` (moving it if attached elsewhere).
    pub fn insert_before(&mut self, reference: NodeId, new: NodeId) -> Result<()> {
        self.check(reference)?;
        if reference == new {
            return Err(node_err("cannot insert a node next to itself"));
        }
        let parent = self.parent(reference).ok_or_else(|| node_err("reference node has no parent"))?;
        self.insert(parent, new, move |_| reference.0)
    }

    /// Insert `new` right after `reference` (moving it if attached elsewhere).
    pub fn insert_after(&mut self, reference: NodeId, new: NodeId) -> Result<()> {
        self.check(reference)?;
        if reference == new {
            return Err(node_err("cannot insert a node next to itself"));
        }
        let parent = self.parent(reference).ok_or_else(|| node_err("reference node has no parent"))?;
        self.insert(parent, new, move |d| d.nodes[reference.0 as usize].next)
    }

    /// Detach `n` (and its subtree) from the tree. It keeps its id and can be inserted again.
    pub fn detach(&mut self, n: NodeId) -> Result<()> {
        self.check(n)?;
        if n.0 == 0 || n.0 == self.root {
            return Err(node_err("cannot detach the document node or the root element"));
        }
        self.unlink(n.0);
        Ok(())
    }

    /// Remove `n` from the tree. Same as [`Document::detach`]; the arena slot is not reused.
    pub fn remove(&mut self, n: NodeId) -> Result<()> {
        self.detach(n)
    }

    // ------------------------------------------------------------------ namespaces

    fn decls_of(&self, e: u32) -> Vec<(String, String)> {
        self.attrs(NodeId(e))
            .filter_map(|(k, v)| xmlns_prefix(k).map(|p| (p.to_owned(), v.into_owned())))
            .filter(|(p, _)| p != "xml" && p != "xmlns")
            .collect()
    }

    fn push_decls(&mut self, e: u32, scope: &mut Vec<(String, u32)>) {
        for (p, uri) in self.decls_of(e) {
            let id = if uri.is_empty() { NS_NONE } else { self.intern_ns(&uri) };
            scope.push((p, id));
        }
    }

    fn intern_ns(&mut self, uri: &str) -> u32 {
        if let Some(&id) = self.ns_map.get(uri) {
            return id;
        }
        let id = self.ns_uris.len() as u32;
        self.ns_uris.push(uri.into());
        self.ns_map.insert(uri.into(), id);
        id
    }

    fn resolve_own_ns(&mut self, n: NodeId) {
        let Some(name) = self.name(n) else { return };
        let prefix = escape::split_qname(name).0.to_owned();
        let id = match self.lookup_namespace(n, &prefix) {
            Some(uri) => self.ns_map.get(uri).copied().unwrap_or(NS_UNBOUND),
            None if prefix.is_empty() => NS_NONE,
            None => NS_UNBOUND,
        };
        if let Data::Element(e) = &mut self.nodes[n.0 as usize].data {
            e.ns = id;
        }
    }

    /// Re-resolve the namespace of every element in the subtree at `r` (after a move or a
    /// change to `xmlns` attributes).
    fn recompute_ns(&mut self, r: u32) {
        let mut scope: Vec<(String, u32)> = Vec::new();
        let ancestors: Vec<u32> = self.ancestors(NodeId(r)).map(|a| a.0).collect();
        for &a in ancestors.iter().rev() {
            if self.is_element_node(NodeId(a)) {
                self.push_decls(a, &mut scope);
            }
        }
        let mut marks: Vec<usize> = Vec::new();
        let mut n = r;
        loop {
            let is_elem = self.is_element_node(NodeId(n));
            if is_elem {
                marks.push(scope.len());
                self.push_decls(n, &mut scope);
                let prefix = self.prefix(NodeId(n)).unwrap_or("");
                let id = resolve(&scope, prefix);
                if let Data::Element(e) = &mut self.nodes[n as usize].data {
                    e.ns = id;
                }
                let first = self.nodes[n as usize].first;
                if first != NIL {
                    n = first;
                    continue;
                }
            }
            loop {
                if self.is_element_node(NodeId(n)) {
                    scope.truncate(marks.pop().unwrap_or(0));
                }
                if n == r {
                    return;
                }
                let node = &self.nodes[n as usize];
                if node.next != NIL {
                    n = node.next;
                    break;
                }
                n = node.parent;
            }
        }
    }
}
