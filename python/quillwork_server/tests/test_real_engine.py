"""The real binding behind the service: round trips, edits, reopen from the store, stale bases,
undo, import/restore and the clean export.

Uses the public synthetic corpus only. Skips when the ``quillwork`` extension isn't built.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from conftest import SYNTHETIC

quillwork = pytest.importorskip("quillwork")

from quillwork.errors import BadVersionError, StaleAnchorError  # noqa: E402

from quillwork_server.core import DocumentService, MemoryStore, QuillworkEngine  # noqa: E402

FILES = sorted(SYNTHETIC.glob("*.docx"))
pytestmark = pytest.mark.skipif(not FILES, reason="no corpus/synthetic/*.docx")


def binding(service: DocumentService, doc_id: str, version: int | None = None) -> Any:
    """The stored version reopened straight through the binding, to look at its text."""
    stored = service.store.get_version(doc_id, version or service.head(doc_id))
    return quillwork.Document.open(stored.data, sidecar=stored.sidecar)


def text_of(doc: Any, handle: str) -> str:
    (line,) = doc.render([handle])
    return str(line.split("] ", 1)[1])


def plain_paragraph(doc: Any, skip: str = "") -> tuple[str, str]:
    """A body paragraph handle (other than ``skip``) with plain text, and its first two words."""
    blocks = json.loads(doc.sidecar())["anchors"]["blocks"]
    for handle, block in sorted(blocks.items(), key=lambda kv: kv[1]["position"]):
        if block["story"] != "b" or handle == skip:
            continue
        words = text_of(doc, handle).split()
        if len(words) >= 3 and all(w.isalpha() for w in words[:2]):
            return handle, " ".join(words[:2])
    raise AssertionError("no plain paragraph")


def replace(handle: str, find: str, new: str, base: int) -> dict[str, Any]:
    op = {"op": "replace_text", "anchor": handle, "find": find, "replace": new}
    return {"base_version": base, "ops": [op], "label": "test edit"}


@pytest.mark.parametrize("path", FILES, ids=lambda p: p.name)
def test_no_op_round_trip_is_byte_identical(path: Path) -> None:
    data = path.read_bytes()
    service = DocumentService(QuillworkEngine(), MemoryStore())
    info = service.upload(data, name=path.name)
    assert info.head == 1
    for variant in ("original", "redline"):
        assert service.export(info.id, variant=variant) == data, variant


@pytest.mark.parametrize("path", FILES, ids=lambda p: p.name)
def test_edit_reopen_stale_and_undo(path: Path) -> None:
    store = MemoryStore()
    service = DocumentService(QuillworkEngine(), store)
    doc_id = service.upload(path.read_bytes(), name=path.name).id
    handle, words = plain_paragraph(binding(service, doc_id))
    original = text_of(binding(service, doc_id), handle)

    result = service.apply(doc_id, replace(handle, words, "Nieuwe tekst", 1), author="Tester")
    assert result["ok"] is True and (result["base_version"], result["version"]) == (1, 2)
    cs = result["change_set"]
    assert service.head(doc_id) == 2
    assert "{++Nieuwe tekst++}" in text_of(binding(service, doc_id), handle)
    assert service.versions(doc_id)[-1].change_set == cs

    # A fresh service on the same store reopens version 2 from the stored .docx + sidecar.
    service = DocumentService(QuillworkEngine(), store)
    with pytest.raises(StaleAnchorError) as exc:
        service.apply(doc_id, replace(handle, words, "Andere tekst", 1))
    assert exc.value.code == "STALE_ANCHOR" and exc.value.anchor == handle
    assert exc.value.version == 2 and service.head(doc_id) == 2
    with pytest.raises(BadVersionError):
        service.apply(doc_id, replace(handle, "Nieuwe", "Ander", 7))

    # Rejecting the change set restores the original text, as version 3.
    undo = service.undo(doc_id, cs)
    assert undo["version"] == 3 and service.head(doc_id) == 3
    assert text_of(binding(service, doc_id), handle) == original
    assert service.versions(doc_id)[-1].kind == "undo"


def test_clean_export_import_and_restore() -> None:
    path = FILES[0]
    data = path.read_bytes()
    service = DocumentService(QuillworkEngine(), MemoryStore())
    doc_id = service.upload(data, name=path.name).id
    handle, words = plain_paragraph(binding(service, doc_id))
    service.apply(doc_id, replace(handle, words, "Nieuwe tekst", 1))

    clean = quillwork.Document.open(service.export(doc_id, variant="clean"))
    assert "Nieuwe tekst" in text_of(clean, handle) and "{++" not in text_of(clean, handle)
    assert service.export(doc_id, variant="redline") != service.export(doc_id, variant="clean")

    # An import opens at head + 1 with the previous sidecar; the anchors carry over.
    service.import_version(doc_id, data)
    assert service.head(doc_id) == 3
    assert text_of(binding(service, doc_id), handle) == text_of(binding(service, doc_id, 1), handle)
    # A restore of version 2 is version 4, and edits continue from there.
    service.restore(doc_id, 2)
    assert service.head(doc_id) == 4
    restored = binding(service, doc_id)
    assert "{++Nieuwe tekst++}" in text_of(restored, handle)
    other, other_words = plain_paragraph(restored, skip=handle)
    result = service.apply(doc_id, replace(other, other_words, "Anders", 4))
    assert (
        result["version"] == 5
        and len({c["id"] for c in binding(service, doc_id).change_sets()}) == 2
    )


FIXTURES = SYNTHETIC.parent / "fixtures"


def _advanced(runner: Any, doc_id: str, op: str, **args: Any) -> Any:
    out = runner.call(doc_id, "advanced_op", {"op": op, "args": json.dumps(args)})
    assert not out.is_error, out.text
    return out


def test_review_lists_changes_and_comments_on_the_real_engine() -> None:
    from quillwork_server.tools import ToolRunner

    service = DocumentService(QuillworkEngine(), MemoryStore())
    runner = ToolRunner(service, author="AI")
    doc_id = service.upload((FIXTURES / "08-tracked-changes.docx").read_bytes(), name="t").id

    data = _advanced(runner, doc_id, "list_changes").structured["data"]
    assert data["change_sets"] == [] and data["revisions"]
    assert {r["author"] for r in data["revisions"]} == {"Other Author"}
    assert not any(r["agent"] for r in data["revisions"])

    handle = data["revisions"][0]["anchors"][0]
    edit = {"anchor": handle, "find": None, "occurrence": None, "style": None, "target": None}
    edit |= {"op": "insert_after", "text": "Added by the agent.", "expect": None}
    out = runner.call(doc_id, "edit", {"base_version": 1, "label": "add", "ops": [edit]})
    assert not out.is_error, out.text
    mine = _advanced(runner, doc_id, "list_changes", author="AI").structured["data"]
    (cs,) = mine["change_sets"]
    assert cs["status"] == "open"
    assert mine["revisions"] and all(r["change_set"] == cs["id"] for r in mine["revisions"])

    targets = {"base_version": 2, "action": "accept", "targets": [cs["id"]]}
    assert not runner.call(doc_id, "review", targets).is_error
    done = _advanced(runner, doc_id, "list_changes", status="accepted").structured["data"]
    assert [c["id"] for c in done["change_sets"]] == [cs["id"]]

    bad = runner.call(doc_id, "advanced_op", {"op": "list_changes", "args": '{"who": "x"}'})
    assert bad.is_error and bad.structured["error"]["path"] == "/who"


def test_comments_lint_and_images_on_the_real_engine() -> None:
    from quillwork_server.tools import ToolRunner

    service = DocumentService(QuillworkEngine(), MemoryStore())
    runner = ToolRunner(service, author="AI")
    doc_id = service.upload((FIXTURES / "07-comments.docx").read_bytes(), name="c").id
    comments = _advanced(runner, doc_id, "list_comments").structured["data"]["comments"]
    assert [len(c["replies"]) for c in comments] == [1, 0]
    reply = {"base_version": 1, "action": "reply", "anchor": comments[1]["id"], "find": None}
    out = runner.call(doc_id, "comment", reply | {"text": "Fixed."})
    assert not out.is_error, out.text
    comments = _advanced(runner, doc_id, "list_comments").structured["data"]["comments"]
    assert [r["text"] for r in comments[1]["replies"]] == ["Fixed."]

    images = service.upload((FIXTURES / "09-images.docx").read_bytes(), name="i").id
    listed = _advanced(runner, images, "list_images").structured["data"]["images"]
    got = _advanced(runner, images, "get_image", image=listed[0]["id"]).structured["data"]
    assert got["asset"] == listed[0]["asset"] and got["base64"]

    fields = service.upload((FIXTURES / "11-fields.docx").read_bytes(), name="f").id
    assert _advanced(runner, fields, "lint").text.splitlines()[1] == "§ lint: none"
