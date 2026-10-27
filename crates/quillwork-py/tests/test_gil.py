"""Open, parse, inflate, serialise and save release the GIL.

Deterministic check: with a 100 s switch interval the interpreter never preempts the main thread,
so a worker thread waiting for the GIL can only run while a native call has released it. The
worker blocks on `go`; main sets `go` and makes the native call; if `done` is set when the call
returns, the GIL was released during it. A control call that keeps the GIL proves the harness
can tell the difference.
"""

from __future__ import annotations

import io
import json
import sys
import threading
import zipfile
from collections.abc import Callable, Iterator

import pytest

import quillwork
from quillwork import ns
from conftest import SYNTHETIC

PARAS = 200_000
BIG_XML = (
    b'<w:document xmlns:w="' + ns.W.encode() + b'"><w:body>'
    + b"<w:p><w:r><w:t>Some text in a run</w:t></w:r></w:p>" * PARAS
    + b"</w:body></w:document>"
)
RELAXED = quillwork.Limits(max_ratio=1_000_000)


def big_docx(method: int) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", method) as z:
        z.writestr("[Content_Types].xml", b'<?xml version="1.0"?><Types xmlns="' + ns.CT.encode() + b'"/>')
        z.writestr("word/document.xml", BIG_XML)
    return buf.getvalue()


@pytest.fixture(scope="module")
def deflated() -> bytes:
    return big_docx(zipfile.ZIP_DEFLATED)


@pytest.fixture(scope="module")
def stored() -> bytes:
    return big_docx(zipfile.ZIP_STORED)


@pytest.fixture
def probe() -> Iterator[Callable[[Callable[[], object], int], bool]]:
    """probe(call, tries) -> True if a waiting thread got to run during one of the calls."""
    old = sys.getswitchinterval()
    sys.setswitchinterval(100.0)
    workers: list[threading.Thread] = []

    def run(call: Callable[[], object], tries: int) -> bool:
        ready, go, done = threading.Event(), threading.Event(), threading.Event()

        def worker() -> None:
            ready.set()
            go.wait()
            done.set()

        t = threading.Thread(target=worker, daemon=True)
        workers.append(t)
        t.start()
        ready.wait()
        go.set()
        for _ in range(tries):
            call()
            if done.is_set():
                return True
        return False

    try:
        yield run
    finally:
        sys.setswitchinterval(old)
        for t in workers:
            t.join(timeout=10)


def test_harness_detects_a_call_that_keeps_the_gil(
    probe: Callable[[Callable[[], object], int], bool],
) -> None:
    doc = quillwork.XmlDocument.parse(BIG_XML)
    # find_all walks the whole tree and builds a list with the GIL held.
    assert probe(lambda: doc.find_all(doc.root, ns.W, "t"), 3) is False


def test_xml_parse_releases(probe: Callable[[Callable[[], object], int], bool]) -> None:
    assert probe(lambda: quillwork.XmlDocument.parse(BIG_XML), 20)


def test_xml_to_bytes_releases(probe: Callable[[Callable[[], object], int], bool]) -> None:
    doc = quillwork.XmlDocument.parse(BIG_XML)
    doc.set_text(doc.find(doc.root, ns.W, "t") or 0, "edited")  # force the slow (walked) path
    assert probe(doc.to_bytes, 20)


def test_package_open_releases(
    probe: Callable[[Callable[[], object], int], bool], stored: bytes
) -> None:
    assert probe(lambda: quillwork.Package.open(stored, RELAXED), 50)


def test_package_part_and_xml_release(
    probe: Callable[[Callable[[], object], int], bool], deflated: bytes
) -> None:
    # A fresh package each call so the inflate cache is cold.
    assert probe(lambda: quillwork.Package.open(deflated, RELAXED).part("word/document.xml"), 20)
    assert probe(lambda: quillwork.Package.open(deflated, RELAXED).xml("word/document.xml"), 20)


def test_package_save_releases(
    probe: Callable[[Callable[[], object], int], bool], deflated: bytes
) -> None:
    pkg = quillwork.Package.open(deflated, RELAXED)
    pkg.set_part("word/document.xml", BIG_XML.replace(b"Some", b"Same", 1))
    assert probe(pkg.save, 20)


def test_concurrent_parses_from_threads_agree() -> None:
    """Many threads parsing at once (GIL released) all get correct, independent results."""
    results: list[int] = []
    lock = threading.Lock()

    def work() -> None:
        doc = quillwork.XmlDocument.parse(BIG_XML)
        n = len(doc.find_all(doc.root, ns.W, "p"))
        with lock:
            results.append(n)

    threads = [threading.Thread(target=work) for _ in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert results == [PARAS] * 4


def test_document_open_apply_and_save_release(
    probe: Callable[[Callable[[], object], int], bool],
) -> None:
    data = SYNTHETIC[-1].read_bytes()
    assert probe(lambda: quillwork.Document.open(data), 20)
    doc = quillwork.Document.open(data)
    blocks = json.loads(doc.sidecar())["anchors"]["blocks"]
    handle = min((h for h, b in blocks.items() if b["story"] == "b"), key=lambda h: blocks[h]["position"])
    (line,) = doc.render([handle])
    first = line.split("] ", 1)[1].split()[0]
    pairs = iter([(first, "Gewijzigd"), ("Gewijzigd", first)] * 20)

    def edit() -> object:
        find, new = next(pairs)
        op = {"op": "replace_text", "anchor": handle, "find": find, "replace": new}
        return doc.apply({"base_version": doc.version, "mode": "direct", "ops": [op]})

    assert probe(edit, 20)
    assert probe(doc.save, 20)
