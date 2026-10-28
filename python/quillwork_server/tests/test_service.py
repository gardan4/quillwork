"""DocumentService on the FakeEngine: versions, CAS, writers, undo, restore, feeds, assets."""

from __future__ import annotations

import threading
from collections.abc import Callable
from dataclasses import replace as replace_record
from pathlib import Path
from typing import Any

import pytest
from quillwork.errors import QuillworkError

from quillwork_server.core import (
    DocumentService,
    FakeEngine,
    FileStore,
    MemoryStore,
    ServiceConfig,
    error_object,
)
from quillwork_server.core.fake import fake_bytes

PNG = b"\x89PNG\r\n\x1a\n" + b"\x00" * 32


def replace(anchor: str, find: str, new: str, base: int) -> dict[str, object]:
    return {
        "base_version": base,
        "ops": [{"op": "replace_text", "anchor": anchor, "find": find, "replace": new}],
    }


def err(exc: pytest.ExceptionInfo[QuillworkError]) -> dict[str, Any]:
    return error_object(exc.value)


def code_of(exc: pytest.ExceptionInfo[QuillworkError]) -> str:
    return str(err(exc)["code"])


def test_upload_creates_version_one(service: DocumentService, doc: str) -> None:
    info = service.info(doc)
    assert (info.id, info.name, info.head) == ("doc1", "contract.docx", 1)
    [rec] = service.versions(doc)
    assert (rec.version, rec.parent, rec.kind, rec.patch) == (1, None, "upload", None)
    assert service.read(doc)["text"].splitlines()[-1] == "[b103] Third para"


def test_apply_makes_a_new_version_with_its_record(service: DocumentService, doc: str) -> None:
    result = service.apply(doc, replace("b101", "world", "there", 1), author="Ann", turn="t1")
    assert (result["ok"], result["version"], result["change_set"]) == (True, 2, "cs_1")
    rec = service.versions(doc)[-1]
    assert (rec.version, rec.parent, rec.kind, rec.change_set) == (2, 1, "apply", "cs_1")
    assert (rec.author, rec.turn) == ("Ann", "t1")
    assert rec.patch == result["patch"]
    assert service.head(doc) == 2
    # the old version is still readable
    assert "Hello world" in service.read(doc, "b101", version=1)["text"]
    assert "Hello there" in service.read(doc, "b101")["text"]


def test_host_author_wins_over_transaction_author(service: DocumentService, doc: str) -> None:
    tx = replace("b101", "world", "x", 1) | {"author": "Mallory"}
    service.apply(doc, tx, author="AI")
    assert service.versions(doc)[-1].author == "AI"


def test_stale_base_on_a_changed_block_is_stale_anchor(service: DocumentService, doc: str) -> None:
    service.apply(doc, replace("b101", "world", "there", 1))
    with pytest.raises(QuillworkError) as exc:
        service.apply(doc, replace("b101", "Hello", "Hi", 1))
    e = err(exc)
    assert (e["code"], e["version"], e["anchor"]) == ("STALE_ANCHOR", 2, "b101")
    assert e["current"] == ["[b101] Hello there"]
    assert service.head(doc) == 2


def test_stale_base_on_an_untouched_block_still_applies(service: DocumentService, doc: str) -> None:
    service.apply(doc, replace("b101", "world", "there", 1))
    result = service.apply(doc, replace("b102", "Second", "2nd", 1))
    assert result["version"] == 3


def test_bad_version_and_missing_document(service: DocumentService, doc: str) -> None:
    with pytest.raises(QuillworkError) as exc:
        service.read(doc, version=9)
    assert code_of(exc) == "BAD_VERSION"
    with pytest.raises(QuillworkError) as exc:
        service.head("nope")
    e = err(exc)
    assert (e["code"], e["what"]) == ("NOT_FOUND", "document")


def test_failed_transaction_leaves_no_version(service: DocumentService, doc: str) -> None:
    with pytest.raises(QuillworkError):
        service.apply(doc, replace("b999", "x", "y", 1))
    assert service.head(doc) == 1
    assert len(service.versions(doc)) == 1


def test_limits(doc: str) -> None:
    svc = DocumentService(
        FakeEngine(), config=ServiceConfig(max_upload_bytes=10, max_ops_per_transaction=1)
    )
    with pytest.raises(QuillworkError) as exc:
        svc.upload(b"x" * 11)
    e = err(exc)
    assert e["code"] == "LIMIT"
    assert e["limit"] == {"name": "uncompressed_bytes", "max": 10, "actual": 11}
    d = svc.upload(b"a\nb\n").id
    tx = replace("b100", "a", "c", 1)
    tx["ops"] = tx["ops"] * 2  # type: ignore[operator]
    with pytest.raises(QuillworkError) as exc:
        svc.apply(d, tx)
    assert err(exc)["limit"]["name"] == "ops_per_transaction"


def test_duplicate_or_bad_document_id(service: DocumentService, doc: str) -> None:
    for bad in (doc, "../x"):
        with pytest.raises(QuillworkError) as exc:
            service.upload(b"x\n", document_id=bad)
        assert code_of(exc) == "INVALID_OP"


def test_undo_rejects_the_change_set_as_a_new_version(service: DocumentService, doc: str) -> None:
    applied = service.apply(doc, replace("b101", "world", "there", 1))
    undone = service.undo(doc, applied["change_set"])
    assert undone["version"] == 3
    rec = service.versions(doc)[-1]
    assert (rec.kind, rec.change_set, rec.label) == ("undo", "cs_1", "undo cs_1")
    assert service.read(doc, "b101")["text"] == "[b101] Hello world"
    # history is kept
    assert "Hello there" in service.read(doc, "b101", version=2)["text"]


def test_restore_makes_an_old_snapshot_the_head(service: DocumentService, doc: str) -> None:
    service.apply(doc, replace("b101", "world", "there", 1) | {"mode": "direct"})
    rec = service.restore(doc, 1, author="Ann")
    assert (rec.version, rec.parent, rec.kind, rec.label) == (3, 2, "restore", "restore v1")
    assert service.export(doc) == service.export(doc, version=1)
    assert service.read(doc, "b101")["text"] == "[b101] Hello world"
    assert service.patches_since(doc, 2).reload


def test_import_version_reloads_viewers(service: DocumentService, doc: str) -> None:
    rec = service.import_version(doc, fake_bytes("# Contract", "Edited in Word"), label="word")
    assert (rec.version, rec.kind, rec.patch) == (2, "import", None)
    feed = service.patches_since(doc, 1)
    assert (feed.version, feed.reload) == (2, True)
    assert "Edited in Word" in service.read(doc)["text"]


def test_patches_since(service: DocumentService, doc: str) -> None:
    r2 = service.apply(doc, replace("b101", "world", "there", 1))
    r3 = service.apply(doc, replace("b102", "Second", "2nd", 2))
    feed = service.patches_since(doc, 1)
    assert (feed.since, feed.version, feed.reload) == (1, 3, False)
    assert feed.patches == [r2["patch"], r3["patch"]]
    assert [p["version"] for p in feed.patches] == [2, 3]
    assert service.patches_since(doc, 3).patches == []
    assert service.patches_since(doc, 0).reload
    assert service.patches_since(doc, 9).reload


def test_patch_feed_is_bounded_to_the_head_sampled_before_a_concurrent_commit() -> None:
    class RacingStore(MemoryStore):
        before_records: Callable[[], None] | None = None

        def records(self, document_id: str, after: int = 0) -> list[Any]:
            callback, self.before_records = self.before_records, None
            if callback is not None:
                callback()
            return super().records(document_id, after)

    store = RacingStore()
    service = DocumentService(FakeEngine(), store)
    doc_id = service.upload(fake_bytes("one", "two")).id
    first = service.apply(doc_id, replace("b100", "one", "ONE", 1))
    store.before_records = lambda: service.apply(doc_id, replace("b101", "two", "TWO", 2))

    feed = service.patches_since(doc_id, 1)

    assert not feed.reload
    assert feed.version == 2
    assert feed.patches == [first["patch"]]
    next_feed = service.patches_since(doc_id, feed.version)
    assert next_feed.version == 3
    assert [p["version"] for p in next_feed.patches] == [3]


@pytest.mark.parametrize("broken", ["gap", "base", "version"])
def test_patch_feed_reloads_when_history_is_not_a_continuous_chain(broken: str) -> None:
    class BrokenHistoryStore(MemoryStore):
        def records(self, document_id: str, after: int = 0) -> list[Any]:
            records = super().records(document_id, after)
            if after == 1:
                if broken == "gap":
                    return records[1:]
                patch = dict(records[0].patch or {})
                patch["base_version" if broken == "base" else "version"] = 99
                records[0] = replace_record(records[0], patch=patch)
            return records

    service = DocumentService(FakeEngine(), BrokenHistoryStore())
    doc_id = service.upload(fake_bytes("one", "two")).id
    service.apply(doc_id, replace("b100", "one", "ONE", 1))
    service.apply(doc_id, replace("b101", "two", "TWO", 2))

    feed = service.patches_since(doc_id, 1)

    assert feed.reload and feed.version == 3 and feed.patches == []


def test_wait_wakes_on_a_new_version(service: DocumentService, doc: str) -> None:
    assert service.wait(doc, 1, 0.01) is None
    got: list[int | None] = []
    t = threading.Thread(target=lambda: got.append(service.wait(doc, 1, 5.0)))
    t.start()
    service.apply(doc, replace("b101", "world", "there", 1))
    t.join(5)
    assert got == [2]


def test_threaded_writers_serialise_without_losing_an_edit(
    service: DocumentService, doc: str
) -> None:
    """8 writers on 4 blocks, each re-reading the head and retrying on a stale anchor."""
    errors: list[BaseException] = []
    barrier = threading.Barrier(8)

    def writer(i: int) -> None:
        anchor = f"b10{i % 4}"
        barrier.wait()
        for _ in range(50):
            base = service.head(doc)
            text = service.read(doc, anchor)["text"].split("] ", 1)[1]
            word = text.split()[0]
            try:
                service.apply(doc, replace(anchor, word, f"{word}{i}", base))
                return
            except QuillworkError as exc:
                if error_object(exc)["code"] not in ("STALE_ANCHOR", "CONFLICT", "NOT_FOUND"):
                    errors.append(exc)
                    return
        errors.append(AssertionError(f"writer {i} never got through"))

    threads = [threading.Thread(target=writer, args=(i,)) for i in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert errors == []
    recs = service.versions(doc)
    assert [r.version for r in recs] == list(range(1, 10))
    assert [r.parent for r in recs[1:]] == list(range(1, 9))
    assert len({r.change_set for r in recs[1:]}) == 8


def test_two_services_on_one_file_store_never_fork_history(tmp_path: Path) -> None:
    """Two processes' worth of services: the store's CAS turns a lost race into CONFLICT/retry."""
    a = DocumentService(FakeEngine(), FileStore(tmp_path))
    b = DocumentService(FakeEngine(), FileStore(tmp_path))
    d = a.upload(fake_bytes("one", "two"), document_id="shared").id
    a.apply(d, replace("b100", "one", "uno", 1))
    # b has no cached copy at v2 yet: it must load v2, not apply on v1
    result = b.apply(d, replace("b101", "two", "dos", 2))
    assert result["version"] == 3
    # a's cached live doc is at v2; its next write must notice the head moved
    result = a.apply(d, replace("b100", "uno", "eins", 3))
    assert result["version"] == 4
    assert [r.parent for r in a.versions(d)[1:]] == [1, 2, 3]
    text = a.read(d)["text"]
    assert "[b100] eins" in text
    assert "[b101] dos" in text


def test_export_variants(service: DocumentService, doc: str) -> None:
    original = service.export(doc)
    service.apply(doc, replace("b101", "world", "there", 1))
    assert service.export(doc, variant="original") == original
    assert service.export(doc, version=1) == original
    assert service.export(doc) != original
    clean = DocumentService(FakeEngine()).upload(service.export(doc, variant="clean")).id
    assert clean


def test_view_model(service: DocumentService, doc: str) -> None:
    vm = service.view_model(doc)
    assert vm["version"] == 1
    assert [b["id"] for b in vm["blocks"]] == ["b100", "b101", "b102", "b103"]


def test_assets_are_typed_from_their_bytes(service: DocumentService, doc: str) -> None:
    aid = service.put_asset(doc, PNG)
    assert aid.startswith("a_")
    asset = service.get_asset(doc, aid)
    assert (asset.data, asset.mime) == (PNG, "image/png")
    with pytest.raises(QuillworkError) as exc:
        service.put_asset(doc, b"<html>")
    assert code_of(exc) == "INVALID_OP"
    with pytest.raises(QuillworkError) as exc:
        service.get_asset(doc, "a_0000000000000000")
    assert code_of(exc) == "NOT_FOUND"


def test_session_tracks_the_version_it_saw(service: DocumentService, doc: str) -> None:
    s = service.session(doc)
    s.read()
    r = s.edit({"op": "replace_text", "anchor": "b101", "find": "world", "replace": "there"})
    assert r["version"] == 2
    r = s.edit({"op": "replace_text", "anchor": "b102", "find": "Second", "replace": "2nd"})
    assert r["version"] == 3
    s.undo(r["change_set"])
    assert service.read(doc, "b101")["text"] == "[b101] Hello there"
    assert service.read(doc, "b102")["text"] == "[b102] Second para"


def test_memory_store_default(doc: str) -> None:
    assert isinstance(DocumentService(FakeEngine()).store, MemoryStore)
