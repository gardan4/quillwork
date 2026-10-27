"""Both stores honour the Store contract: versions, compare-and-swap, assets."""

from __future__ import annotations

import threading
from pathlib import Path

import pytest

from quillwork_server.core import (
    Asset,
    DocumentExists,
    DocumentNotFound,
    FileStore,
    HeadMoved,
    MemoryStore,
    Store,
    VersionNotFound,
    VersionRecord,
)


@pytest.fixture(params=["memory", "file"])
def store(request: pytest.FixtureRequest, tmp_path: Path) -> Store:
    return MemoryStore() if request.param == "memory" else FileStore(tmp_path / "docs")


def record(version: int, parent: int | None = None, **kw: object) -> VersionRecord:
    kind = "upload" if version == 1 else "apply"
    return VersionRecord("d1", version, parent, kind, 1000.0 + version, **kw)  # type: ignore[arg-type]


def test_create_and_read_back(store: Store) -> None:
    info = store.create("d1", "a.docx", b"v1", {"k": 1}, record(1, label="a.docx"))
    assert (info.id, info.name, info.head) == ("d1", "a.docx", 1)
    got = store.get_version("d1", 1)
    assert got.data == b"v1"
    assert got.sidecar == {"k": 1}
    assert got.record.label == "a.docx"
    assert [d.id for d in store.list_documents()] == ["d1"]


def test_create_twice_is_refused(store: Store) -> None:
    store.create("d1", "a", b"x", None, record(1))
    with pytest.raises(DocumentExists):
        store.create("d1", "a", b"y", None, record(1))


def test_put_version_is_compare_and_swap(store: Store) -> None:
    store.create("d1", "a", b"v1", None, record(1))
    store.put_version("d1", 1, b"v2", None, record(2, 1, change_set="cs_1", patch={"p": 1}))
    assert store.head("d1") == 2
    with pytest.raises(HeadMoved) as moved:
        store.put_version("d1", 1, b"v2b", None, record(2, 1))
    assert moved.value.actual == 2
    assert store.get_version("d1", 2).data == b"v2"
    recs = store.records("d1", after=1)
    assert [(r.version, r.parent, r.change_set, r.patch) for r in recs] == [
        (2, 1, "cs_1", {"p": 1})
    ]
    assert [r.version for r in store.records("d1")] == [1, 2]


def test_missing_document_and_version(store: Store) -> None:
    with pytest.raises(DocumentNotFound):
        store.head("nope")
    with pytest.raises(DocumentNotFound):
        store.put_version("nope", 1, b"", None, record(2, 1))
    store.create("d1", "a", b"v1", None, record(1))
    with pytest.raises(VersionNotFound):
        store.get_version("d1", 7)


def test_bad_ids_never_touch_the_filesystem(tmp_path: Path) -> None:
    store = FileStore(tmp_path / "docs")
    for bad in ("../escape", "a/b", "", ".hidden"):
        with pytest.raises((ValueError, DocumentNotFound)):
            store.create(bad, "x", b"", None, record(1))
        with pytest.raises(DocumentNotFound):
            store.head(bad)
    assert not (tmp_path / "escape").exists()


def test_assets(store: Store) -> None:
    store.create("d1", "a", b"v1", None, record(1))
    assert store.get_asset("d1", "a_0123456789abcdef") is None
    store.put_asset("d1", "a_0123456789abcdef", Asset(b"\x89PNG", "image/png"))
    assert store.get_asset("d1", "a_0123456789abcdef") == Asset(b"\x89PNG", "image/png")


def test_concurrent_cas_lets_exactly_one_writer_win(store: Store) -> None:
    store.create("d1", "a", b"v1", None, record(1))
    wins: list[int] = []
    barrier = threading.Barrier(8)

    def writer(i: int) -> None:
        barrier.wait()
        try:
            store.put_version("d1", 1, f"w{i}".encode(), None, record(2, 1))
            wins.append(i)
        except HeadMoved:
            pass

    threads = [threading.Thread(target=writer, args=(i,)) for i in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert len(wins) == 1
    assert store.get_version("d1", 2).data == f"w{wins[0]}".encode()


def test_file_store_survives_a_new_instance(tmp_path: Path) -> None:
    FileStore(tmp_path).create("d1", "a", b"v1", {"s": 1}, record(1))
    FileStore(tmp_path).put_version("d1", 1, b"v2", None, record(2, 1))
    again = FileStore(tmp_path)
    assert again.head("d1") == 2
    assert again.get_version("d1", 1).sidecar == {"s": 1}
