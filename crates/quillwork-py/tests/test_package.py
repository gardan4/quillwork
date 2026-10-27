"""Package: byte-identical round trip, edit locality, part/rels/content-type API."""

from __future__ import annotations

import io
import zipfile
from pathlib import Path

import pytest
from conftest import PRIVATE, SYNTHETIC, corpus_params, raw_entries

import quillwork
from quillwork import ns

CORPUS = corpus_params()
# Note: assertions below compare via booleans (`same = a == b; assert same, label`) so a failure
# on a private document never prints its bytes or text.


def test_corpus_present() -> None:
    assert len(SYNTHETIC) >= 2


@pytest.mark.parametrize("path", [p for _, p in CORPUS], ids=[i for i, _ in CORPUS])
def test_round_trip_is_byte_identical(path: Path) -> None:
    data = path.read_bytes()
    pkg = quillwork.Package.open(data)
    assert not pkg.is_modified
    same = pkg.save() == data
    assert same, "unmodified save differs from input"
    # Every part reads (CRC-checked inflate) and every XML part round-trips through the XML layer.
    xml_ok = 0
    for name in pkg.names():
        body = pkg.part(name)
        if name.endswith((".xml", ".rels")):
            doc = quillwork.XmlDocument.parse(body)
            same = doc.to_bytes() == body
            assert same, "xml part round trip differs"
            xml_ok += 1
    assert xml_ok > 0


@pytest.mark.parametrize("path", [p for _, p in CORPUS], ids=[i for i, _ in CORPUS])
def test_edit_is_local(path: Path) -> None:
    """Change the first w:t: only word/document.xml changes; in it, only that element's bytes."""
    data = path.read_bytes()
    pkg = quillwork.Package.open(data)
    part = "word/document.xml"
    source = pkg.part(part)
    doc = pkg.xml(part)
    t = doc.find(doc.root, ns.W, "t")
    assert t is not None
    span = doc.source_span(t)
    assert span is not None
    doc.set_text(t, "Quillwork & <edit>")
    pkg.set_xml(part, doc)
    assert pkg.is_modified
    out = pkg.save()

    before, after = raw_entries(data), raw_entries(out)
    same_order = list(before) == list(after)
    assert same_order, "entry order changed"
    changed = [n for n in before if before[n] != after[n]]
    assert len(changed) == 1 and changed[0] == part, f"{len(changed)} entries changed"

    edited = quillwork.Package.open(out).part(part)
    start, end = span
    tail = len(source) - end
    prefix_kept = edited[:start] == source[:start]
    suffix_kept = edited[len(edited) - tail :] == source[end:]
    assert prefix_kept and suffix_kept, "bytes outside the edited element changed"
    middle = edited[start : len(edited) - tail]
    escaped = middle.endswith(b">Quillwork &amp; &lt;edit&gt;</w:t>")
    assert escaped, "edited element not written as expected"

    # The result is still a valid zip that the stdlib reads, CRCs included.
    with zipfile.ZipFile(io.BytesIO(out)) as z:
        assert z.testzip() is None


def test_part_api(tmp_path: Path) -> None:
    pkg = quillwork.Package.open(SYNTHETIC[0].read_bytes())
    names = pkg.names()
    assert "[Content_Types].xml" in names and "word/document.xml" in pkg
    assert "/word/document.xml" in pkg and "WORD/Document.xml" in pkg  # OPC names: case-insensitive
    assert len(pkg) == len(names)
    assert pkg.content_type_of("word/document.xml") == (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
    )
    assert pkg.content_type_of("nope.bin") is None

    rels = pkg.rels_of("")
    main = [r for r in rels if r.rel_type == ns.RT_OFFICE_DOCUMENT]
    assert len(main) == 1 and pkg.resolve("", main[0]) == "word/document.xml"
    assert all(isinstance(r.id, str) and not r.external for r in rels)
    assert main[0] in set(pkg.rels_of(""))  # frozen, eq + hash
    doc_rels = pkg.rels_of("word/document.xml")
    assert any(r.rel_type.endswith("/styles") for r in doc_rels)

    with pytest.raises(quillwork.NotFoundError) as exc:
        pkg.part("word/missing.xml")
    assert isinstance(exc.value, LookupError) and exc.value.code == "NOT_FOUND"

    # add / set / remove
    pkg.add_part("customXml/item9.xml", b"<a/>", "application/xml")
    assert pkg.part("customXml/item9.xml") == b"<a/>"
    pkg.set_part("customXml/item9.xml", bytearray(b"<b/>"))
    pkg.remove_part("docProps/app.xml")
    assert "docProps/app.xml" not in pkg
    out = pkg.save()
    reopened = quillwork.Package.open(out)
    assert reopened.part("customXml/item9.xml") == b"<b/>"
    assert reopened.content_type_of("customXml/item9.xml") == "application/xml"
    assert "docProps/app.xml" not in reopened
    with zipfile.ZipFile(io.BytesIO(out)) as z:
        assert z.testzip() is None


def test_limits_object() -> None:
    d = quillwork.Limits()
    assert d.max_entries == 10_000 and d.max_ratio == 100
    custom = quillwork.Limits(max_entries=5)
    assert custom.max_entries == 5 and custom.max_ratio == d.max_ratio
    assert custom == quillwork.Limits(max_entries=5) and custom != d
    assert "max_entries=5" in repr(custom)


def test_private_corpus_reported_by_count_only() -> None:
    """Not an assertion about content: records how many private files ran (0 = absent)."""
    print(f"private corpus files: {len(PRIVATE)}")
