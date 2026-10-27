"""XmlDocument: lossless parse/serialise, navigation, edits, error mapping."""

from __future__ import annotations

import pytest

import quillwork
from quillwork import XmlDocument, ns

SRC = (
    b'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\r\n'
    b'<w:document xmlns:w="' + ns.W.encode() + b'" xmlns:r="' + ns.R.encode() + b'">'
    b"<w:body>"
    b'<w:p><w:r><w:t xml:space="preserve">Hello  </w:t></w:r><w:r><w:t>world</w:t></w:r></w:p>'
    b"<!-- note -->"
    b"<w:p><w:r><w:t>A &amp; B</w:t></w:r></w:p>"
    b"</w:body></w:document>"
)


def test_lossless_and_navigation() -> None:
    doc = XmlDocument.parse(SRC)
    assert doc.to_bytes() == SRC and not doc.is_modified
    root = doc.root
    assert doc.name(root) == "w:document" and doc.local_name(root) == "document"
    assert doc.prefix(root) == "w" and doc.namespace(root) == ns.W
    assert doc.matches(root, ns.W, "document") and doc.kind(root) == "element"
    assert doc.kind(doc.document_node) == "document" and doc.parent(root) == doc.document_node
    assert doc.lookup_namespace(root, "r") == ns.R

    body = doc.find_child(root, ns.W, "body")
    assert body is not None
    paras = doc.child_elements(body)
    assert len(paras) == 2
    assert [doc.kind(c) for c in doc.children(body)] == ["element", "comment", "element"]
    comment = doc.children(body)[1]
    assert doc.text(comment) == " note "
    assert doc.next_sibling(paras[0]) == comment and doc.prev_sibling(comment) == paras[0]
    assert doc.first_child(body) == paras[0] and doc.last_child(body) == paras[1]

    ts = doc.find_all(root, ns.W, "t")
    assert [doc.text_content(t) for t in ts] == ["Hello  ", "world", "A & B"]
    assert doc.find(root, ns.W, "t") == ts[0]
    assert doc.text(ts[0]) is None  # elements have no leaf value
    assert doc.text(doc.children(ts[2])[0]) == "A & B"
    assert doc.attr(ts[0], "xml:space") == "preserve"
    assert doc.attr_ns(ts[0], ns.XML, "space") == "preserve"
    assert doc.attrs(ts[0]) == [("xml:space", "preserve")]
    assert doc.ancestors(ts[0])[:3] == [doc.parent(ts[0]), paras[0], body]
    assert ts[0] in doc.descendants(paras[0])
    assert doc.node_to_bytes(ts[2]) == b"<w:t>A &amp; B</w:t>"
    assert doc.source_span(ts[2]) is not None
    assert doc.node_count > 10


def test_edits() -> None:
    doc = XmlDocument.parse(SRC)
    ts = doc.find_all(doc.root, ns.W, "t")
    doc.set_text(ts[1], "<there>")
    assert doc.is_modified
    out = doc.to_bytes()
    assert b"<w:t>&lt;there&gt;</w:t>" in out
    assert out.replace(b"<w:t>&lt;there&gt;</w:t>", b"<w:t>world</w:t>") == SRC

    doc.set_attr(ts[1], "xml:space", "preserve")
    assert doc.remove_attr(ts[1], "xml:space") is True
    assert doc.remove_attr(ts[1], "xml:space") is False

    body = doc.find(doc.root, ns.W, "body")
    assert body is not None
    p = doc.create_element("w:p")
    r = doc.create_element("w:r")
    t = doc.create_element("w:t")
    doc.append_child(p, r)
    doc.append_child(r, t)
    doc.append_child(t, doc.create_text("new"))
    doc.append_child(body, p)
    copy = doc.clone_subtree(p)
    doc.insert_before(doc.child_elements(body)[0], copy)
    reparsed = XmlDocument.parse(doc.to_bytes())
    texts = [reparsed.text_content(x) for x in reparsed.find_all(reparsed.root, ns.W, "t")]
    assert texts == ["new", "Hello  ", "<there>", "A & B", "new"]

    doc.remove(copy)
    assert doc.parent(copy) is None


def test_bad_node_ids_never_crash() -> None:
    doc = XmlDocument.parse(SRC)
    bogus = 10**6
    assert doc.kind(bogus) is None and doc.name(bogus) is None and doc.parent(bogus) is None
    assert doc.children(bogus) == [] and doc.text_content(bogus) == ""
    with pytest.raises(quillwork.XmlNodeError) as exc:
        doc.set_text(bogus, "x")
    assert exc.value.code == "INVALID_OP" and isinstance(exc.value, ValueError)
    with pytest.raises(OverflowError):
        doc.kind(-1)


def test_bad_names_and_values() -> None:
    doc = XmlDocument.parse(SRC)
    with pytest.raises(quillwork.XmlValueError) as exc:
        doc.create_element("not a name")
    assert exc.value.code == "INVALID_OP"


def test_parse_errors() -> None:
    with pytest.raises(quillwork.XmlParseError) as exc:
        XmlDocument.parse(b"<a><b></a>")
    assert exc.value.code == "BAD_PACKAGE" and isinstance(exc.value.offset, int)
    assert isinstance(exc.value, quillwork.BadPackageError)

    with pytest.raises(quillwork.XmlDoctypeError) as dt:
        XmlDocument.parse(b'<?xml version="1.0"?><!DOCTYPE a [<!ENTITY x "y">]><a>&x;</a>')
    assert dt.value.offset is not None and isinstance(dt.value, quillwork.XmlParseError)

    deep = b"<a>" * 300 + b"</a>" * 300
    with pytest.raises(quillwork.LimitError):
        XmlDocument.parse(deep)
    assert XmlDocument.parse(deep, quillwork.XmlLimits(max_depth=400)).to_bytes() == deep
    with pytest.raises(quillwork.LimitError):
        XmlDocument.parse(SRC, quillwork.XmlLimits(max_nodes=5))
