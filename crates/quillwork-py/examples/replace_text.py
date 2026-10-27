"""Replace the text of the first run in a .docx as a tracked change, and prove nothing else moved.

    python examples/replace_text.py IN.docx [OUT.docx] [NEW_TEXT] [--direct]

The run ``<w:r><w:rPr/><w:t>old</w:t></w:r>`` becomes

    <w:del ...><w:r><w:rPr/><w:delText>old</w:delText></w:r></w:del>
    <w:ins ...><w:r><w:rPr/><w:t>new</w:t></w:r></w:ins>

with the run's formatting (``w:rPr``) kept byte for byte on both sides. ``--direct`` overwrites
the text in place instead. Every zip entry other than the main document part must come out
byte-identical: same compressed bytes, same local header. Type-checked with ``mypy --strict``.
This is the DESIGN §2 binding-ergonomics probe; see docs/BINDING.md for the verdict.
"""

from __future__ import annotations

import io
import struct
import sys
import zipfile
from datetime import UTC, datetime
from pathlib import Path

import quillwork
from quillwork import ns

AUTHOR = "Quillwork"


def raw_entries(data: bytes) -> dict[str, bytes]:
    """Each entry's local header + compressed data, exactly as stored."""
    out: dict[str, bytes] = {}
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        for info in z.infolist():
            start = info.header_offset
            name_len, extra_len = struct.unpack_from("<HH", data, start + 26)
            end = start + 30 + name_len + extra_len + info.compress_size
            out[info.filename] = data[start:end]
    return out


def main_part(pkg: quillwork.Package) -> str:
    """The main document part, found the way Word does: through _rels/.rels."""
    for rel in pkg.rels_of(""):
        if rel.rel_type == ns.RT_OFFICE_DOCUMENT:
            target = pkg.resolve("", rel)
            if target is not None:
                return target
    raise quillwork.NotFoundError("no officeDocument relationship")


def first_plain_run(doc: quillwork.XmlDocument) -> tuple[int, int]:
    """(w:r, w:t) of the first run with text that is not already inside a revision."""
    for t in doc.find_all(doc.root, ns.W, "t"):
        run = doc.parent(t)
        if run is None or not doc.matches(run, ns.W, "r"):
            continue
        if any(doc.matches(a, ns.W, "ins") or doc.matches(a, ns.W, "del") for a in doc.ancestors(run)):
            continue
        return run, t
    raise quillwork.NotFoundError("no plain w:r/w:t in the body")


def next_revision_id(doc: quillwork.XmlDocument) -> int:
    """One past the highest numeric w:id in the part (revisions, comments, bookmarks share it)."""
    highest = 0
    for n in doc.descendants(doc.document_node):
        value = doc.attr_ns(n, ns.W, "id")
        if value is not None and value.isdigit():
            highest = max(highest, int(value))
    return highest + 1


def tracked_replace(doc: quillwork.XmlDocument, run: int, new_text: str) -> None:
    """Wrap `run` in w:del (its w:t renamed to w:delText) and insert a w:ins run after it."""
    w = doc.prefix(run) or "w"
    rev = next_revision_id(doc)
    date = datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")

    def revision(tag: str, rev_id: int) -> int:
        el = doc.create_element(f"{w}:{tag}")
        doc.set_attr(el, f"{w}:id", str(rev_id))
        doc.set_attr(el, f"{w}:author", AUTHOR)
        doc.set_attr(el, f"{w}:date", date)
        return el

    # New run first, cloned from the untouched original so its w:rPr keeps its source bytes.
    new_run = doc.clone_subtree(run)
    for child in doc.child_elements(new_run):
        if not doc.matches(child, ns.W, "rPr"):
            doc.remove(child)
    t = doc.create_element(f"{w}:t")
    doc.set_text(t, new_text)
    if new_text != new_text.strip():
        doc.set_attr(t, "xml:space", "preserve")
    doc.append_child(new_run, t)

    deleted = revision("del", rev)
    inserted = revision("ins", rev + 1)
    doc.insert_before(run, deleted)
    doc.insert_after(deleted, inserted)
    doc.append_child(inserted, new_run)
    doc.append_child(deleted, run)  # moves the original run inside w:del
    for old_t in doc.find_all(run, ns.W, "t"):
        doc.set_name(old_t, f"{w}:delText")


def replace_first_text(data: bytes, new_text: str, *, tracked: bool = True) -> tuple[bytes, str, str]:
    """Returns (new .docx bytes, edited part name, old text)."""
    pkg = quillwork.Package.open(data)
    part = main_part(pkg)
    doc = pkg.xml(part)
    run, t = first_plain_run(doc)
    old = doc.text_content(t)
    if tracked:
        tracked_replace(doc, run, new_text)
    else:
        doc.set_text(t, new_text)
    pkg.set_xml(part, doc)
    return pkg.save(), part, old


def check(before: bytes, after: bytes, part: str, old: str, new_text: str, *, tracked: bool) -> int:
    """Assert the edit landed and nothing else moved; returns the number of untouched entries."""
    a, b = raw_entries(before), raw_entries(after)
    assert list(a) == list(b), "entry order changed"
    changed = [name for name in a if a[name] != b[name]]
    assert changed == [part], f"unexpected changes: {changed}"

    doc = quillwork.Package.open(after).xml(part)
    if not tracked:
        first = doc.find(doc.root, ns.W, "t")
        assert first is not None and doc.text_content(first) == new_text
        return len(a) - 1

    ins = doc.find(doc.root, ns.W, "ins")
    dele = doc.find(doc.root, ns.W, "del")
    assert ins is not None and dele is not None, "no w:ins / w:del written"
    assert doc.next_sibling(dele) == ins, "w:ins does not follow w:del"
    assert doc.text_content(ins) == new_text
    assert "".join(doc.text_content(d) for d in doc.find_all(dele, ns.W, "delText")) == old
    # Formatting kept: both runs carry the same w:rPr bytes (or neither has one).
    rpr = [doc.find_child(r, ns.W, "rPr") for r in (doc.find_child(dele, ns.W, "r"), doc.find_child(ins, ns.W, "r")) if r is not None]
    assert len(rpr) == 2, "revision runs missing"
    old_rpr, new_rpr = rpr
    assert (old_rpr is None) == (new_rpr is None)
    if old_rpr is not None and new_rpr is not None:
        assert doc.node_to_bytes(old_rpr) == doc.node_to_bytes(new_rpr)
    return len(a) - 1


def main(argv: list[str]) -> int:
    tracked = "--direct" not in argv
    args = [a for a in argv if a != "--direct"]
    if len(args) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    src = Path(args[1])
    dst = Path(args[2]) if len(args) > 2 else src.with_name(src.stem + ".edited.docx")
    new_text = args[3] if len(args) > 3 else "Quillwork was here"

    before = src.read_bytes()
    after, part, old = replace_first_text(before, new_text, tracked=tracked)
    untouched = check(before, after, part, old, new_text, tracked=tracked)

    dst.write_bytes(after)
    how = "tracked change" if tracked else "direct edit"
    print(f"{part}: first run {len(old)} -> {len(new_text)} chars as a {how}; {untouched} other entries byte-identical")
    print(f"wrote {dst} ({len(after)} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
