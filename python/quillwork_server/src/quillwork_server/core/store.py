"""Version storage: every version of every document as bytes + sidecar JSON + a record.

The :class:`Store` protocol is what a host implements (for example Postgres rows + S3 blobs).
Two reference implementations ship here: :class:`MemoryStore` (tests, single process) and
:class:`FileStore` (a directory per document; safe across processes on POSIX through ``flock``).

Concurrency contract: :meth:`Store.put_version` is a compare-and-swap on the head version. It
stores version ``expected_head + 1`` only while the head is still ``expected_head`` and raises
:class:`HeadMoved` otherwise. The service's per-document lock makes the CAS failure rare; the CAS
makes it harmless when a host's lock is advisory or misconfigured.
"""

from __future__ import annotations

import json
import os
import re
import threading
import time
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Protocol, runtime_checkable

from .types import JSON, Asset, DocumentInfo, StoredVersion, VersionRecord

try:  # POSIX advisory file locks for FileStore; Windows falls back to the in-process lock.
    import fcntl
except ImportError:  # pragma: no cover - not on the test platforms
    fcntl = None  # type: ignore[assignment]

#: Document ids are path segments and URL segments: keep them boring.
DOCUMENT_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
ASSET_ID = re.compile(r"^a_[0-9a-f]{16}$")


class StoreError(Exception):
    """Base of the store's own exceptions (the service maps them to spec errors)."""


class DocumentNotFound(StoreError, LookupError):
    def __init__(self, document_id: str) -> None:
        super().__init__(f"no document {document_id!r}")
        self.document_id = document_id


class DocumentExists(StoreError):
    def __init__(self, document_id: str) -> None:
        super().__init__(f"document {document_id!r} already exists")
        self.document_id = document_id


class VersionNotFound(StoreError, LookupError):
    def __init__(self, document_id: str, version: int, head: int) -> None:
        super().__init__(f"document {document_id!r} has no version {version} (head is {head})")
        self.document_id = document_id
        self.version = version
        self.head = head


class HeadMoved(StoreError):
    """The compare-and-swap in :meth:`Store.put_version` lost: another writer committed first."""

    def __init__(self, document_id: str, expected: int, actual: int) -> None:
        super().__init__(f"document {document_id!r}: head is v{actual}, expected v{expected}")
        self.document_id = document_id
        self.expected = expected
        self.actual = actual


def check_document_id(document_id: str) -> str:
    if not DOCUMENT_ID.match(document_id) or ".." in document_id:
        raise ValueError(f"invalid document id {document_id!r}")
    return document_id


@runtime_checkable
class Store(Protocol):
    """Where versions live. Every method is synchronous and thread-safe."""

    def create(
        self,
        document_id: str,
        name: str,
        data: bytes,
        sidecar: JSON | None,
        record: VersionRecord,
    ) -> DocumentInfo:
        """Store version 1 of a new document. Raises :class:`DocumentExists`."""
        ...

    def info(self, document_id: str) -> DocumentInfo:
        """Raises :class:`DocumentNotFound`."""
        ...

    def head(self, document_id: str) -> int:
        """The current version. Raises :class:`DocumentNotFound`."""
        ...

    def list_documents(self) -> list[DocumentInfo]: ...

    def put_version(
        self,
        document_id: str,
        expected_head: int,
        data: bytes,
        sidecar: JSON | None,
        record: VersionRecord,
    ) -> None:
        """Store ``record.version`` (``expected_head + 1``) if the head is ``expected_head``.

        Raises :class:`HeadMoved` when it is not, :class:`DocumentNotFound` when there is no
        such document.
        """
        ...

    def get_version(self, document_id: str, version: int) -> StoredVersion:
        """Raises :class:`DocumentNotFound` or :class:`VersionNotFound`."""
        ...

    def records(self, document_id: str, after: int = 0) -> list[VersionRecord]:
        """Records of the versions newer than ``after``, oldest first."""
        ...

    def put_asset(self, document_id: str, asset_id: str, asset: Asset) -> None: ...

    def get_asset(self, document_id: str, asset_id: str) -> Asset | None: ...


def _check_cas(document_id: str, expected_head: int, head: int, record: VersionRecord) -> None:
    if head != expected_head:
        raise HeadMoved(document_id, expected_head, head)
    if record.version != expected_head + 1:
        raise ValueError(f"record.version must be {expected_head + 1}, got {record.version}")


class MemoryStore:
    """Everything in dicts behind one lock. For tests and single-process hosts."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._docs: dict[str, DocumentInfo] = {}
        self._versions: dict[str, list[StoredVersion]] = {}
        self._assets: dict[tuple[str, str], Asset] = {}

    def create(
        self,
        document_id: str,
        name: str,
        data: bytes,
        sidecar: JSON | None,
        record: VersionRecord,
    ) -> DocumentInfo:
        check_document_id(document_id)
        with self._lock:
            if document_id in self._docs:
                raise DocumentExists(document_id)
            info = DocumentInfo(document_id, name, record.version, record.created_at)
            self._docs[document_id] = info
            self._versions[document_id] = [StoredVersion(record, bytes(data), _copy(sidecar))]
            return info

    def info(self, document_id: str) -> DocumentInfo:
        with self._lock:
            try:
                return self._docs[document_id]
            except KeyError:
                raise DocumentNotFound(document_id) from None

    def head(self, document_id: str) -> int:
        return self.info(document_id).head

    def list_documents(self) -> list[DocumentInfo]:
        with self._lock:
            return sorted(self._docs.values(), key=lambda d: (d.created_at, d.id))

    def put_version(
        self,
        document_id: str,
        expected_head: int,
        data: bytes,
        sidecar: JSON | None,
        record: VersionRecord,
    ) -> None:
        with self._lock:
            info = self._docs.get(document_id)
            if info is None:
                raise DocumentNotFound(document_id)
            _check_cas(document_id, expected_head, info.head, record)
            self._versions[document_id].append(StoredVersion(record, bytes(data), _copy(sidecar)))
            self._docs[document_id] = DocumentInfo(
                info.id, info.name, record.version, info.created_at
            )

    def get_version(self, document_id: str, version: int) -> StoredVersion:
        with self._lock:
            versions = self._versions.get(document_id)
            if versions is None:
                raise DocumentNotFound(document_id)
            if not 1 <= version <= len(versions):
                raise VersionNotFound(document_id, version, len(versions))
            stored = versions[version - 1]
            return StoredVersion(stored.record, stored.data, _copy(stored.sidecar))

    def records(self, document_id: str, after: int = 0) -> list[VersionRecord]:
        with self._lock:
            versions = self._versions.get(document_id)
            if versions is None:
                raise DocumentNotFound(document_id)
            return [v.record for v in versions[max(0, after) :]]

    def put_asset(self, document_id: str, asset_id: str, asset: Asset) -> None:
        if not ASSET_ID.match(asset_id):
            raise ValueError(f"invalid asset id {asset_id!r}")
        with self._lock:
            if document_id not in self._docs:
                raise DocumentNotFound(document_id)
            self._assets[(document_id, asset_id)] = asset

    def get_asset(self, document_id: str, asset_id: str) -> Asset | None:
        with self._lock:
            return self._assets.get((document_id, asset_id))


class FileStore:
    """A directory per document::

        <root>/<id>/meta.json                {id, name, created_at}
        <root>/<id>/HEAD                     current version number
        <root>/<id>/v<N>.docx                full snapshot of version N
        <root>/<id>/v<N>.json                {"record": ..., "sidecar": ...}
        <root>/<id>/assets/<asset id>        uploaded image bytes (+ .mime)

    Files are written to a temporary name and renamed, so a reader never sees half a version.
    ``put_version`` holds an exclusive ``flock`` on ``<id>/.lock`` around read-HEAD, write, and
    write-HEAD, which makes the compare-and-swap hold across processes on one host.
    """

    def __init__(self, root: str | os.PathLike[str]) -> None:
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True)
        self._local = threading.Lock()

    # -- helpers -----------------------------------------------------------------------------

    def _dir(self, document_id: str) -> Path:
        return self.root / check_document_id(document_id)

    def _existing(self, document_id: str) -> Path:
        try:
            d = self._dir(document_id)
        except ValueError:
            raise DocumentNotFound(document_id) from None
        if not (d / "HEAD").is_file():
            raise DocumentNotFound(document_id)
        return d

    @contextmanager
    def _locked(self, d: Path) -> Iterator[None]:
        with self._local:
            d.mkdir(parents=True, exist_ok=True)
            with open(d / ".lock", "a+b") as fh:
                if fcntl is not None:
                    fcntl.flock(fh.fileno(), fcntl.LOCK_EX)
                try:
                    yield
                finally:
                    if fcntl is not None:
                        fcntl.flock(fh.fileno(), fcntl.LOCK_UN)

    @staticmethod
    def _write(path: Path, data: bytes) -> None:
        tmp = path.with_name(f".{path.name}.{os.getpid()}.{threading.get_ident()}.tmp")
        with open(tmp, "wb") as fh:
            fh.write(data)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp, path)

    def _write_version(
        self, d: Path, data: bytes, sidecar: JSON | None, record: VersionRecord
    ) -> None:
        self._write(d / f"v{record.version}.docx", data)
        meta = {"record": record.to_dict(), "sidecar": sidecar}
        self._write(d / f"v{record.version}.json", _dumps(meta))
        self._write(d / "HEAD", str(record.version).encode())

    @staticmethod
    def _read_head(d: Path) -> int:
        return int((d / "HEAD").read_text().strip())

    def _info(self, d: Path) -> DocumentInfo:
        meta = json.loads((d / "meta.json").read_text(encoding="utf-8"))
        return DocumentInfo(
            str(meta["id"]), str(meta["name"]), self._read_head(d), float(meta["created_at"])
        )

    # -- Store -------------------------------------------------------------------------------

    def create(
        self,
        document_id: str,
        name: str,
        data: bytes,
        sidecar: JSON | None,
        record: VersionRecord,
    ) -> DocumentInfo:
        d = self._dir(document_id)
        with self._locked(d):
            if (d / "HEAD").exists():
                raise DocumentExists(document_id)
            meta = {"id": document_id, "name": name, "created_at": record.created_at}
            self._write(d / "meta.json", _dumps(meta))
            self._write_version(d, data, sidecar, record)
            return DocumentInfo(document_id, name, record.version, record.created_at)

    def info(self, document_id: str) -> DocumentInfo:
        return self._info(self._existing(document_id))

    def head(self, document_id: str) -> int:
        return self._read_head(self._existing(document_id))

    def list_documents(self) -> list[DocumentInfo]:
        out = []
        for d in self.root.iterdir():
            if d.is_dir() and (d / "HEAD").is_file() and (d / "meta.json").is_file():
                out.append(self._info(d))
        return sorted(out, key=lambda i: (i.created_at, i.id))

    def put_version(
        self,
        document_id: str,
        expected_head: int,
        data: bytes,
        sidecar: JSON | None,
        record: VersionRecord,
    ) -> None:
        d = self._existing(document_id)
        with self._locked(d):
            _check_cas(document_id, expected_head, self._read_head(d), record)
            self._write_version(d, data, sidecar, record)

    def get_version(self, document_id: str, version: int) -> StoredVersion:
        d = self._existing(document_id)
        head = self._read_head(d)
        if not 1 <= version <= head:
            raise VersionNotFound(document_id, version, head)
        meta = json.loads((d / f"v{version}.json").read_text(encoding="utf-8"))
        data = (d / f"v{version}.docx").read_bytes()
        return StoredVersion(VersionRecord.from_dict(meta["record"]), data, meta.get("sidecar"))

    def records(self, document_id: str, after: int = 0) -> list[VersionRecord]:
        d = self._existing(document_id)
        head = self._read_head(d)
        out = []
        for v in range(max(0, after) + 1, head + 1):
            meta = json.loads((d / f"v{v}.json").read_text(encoding="utf-8"))
            out.append(VersionRecord.from_dict(meta["record"]))
        return out

    def put_asset(self, document_id: str, asset_id: str, asset: Asset) -> None:
        if not ASSET_ID.match(asset_id):
            raise ValueError(f"invalid asset id {asset_id!r}")
        d = self._existing(document_id) / "assets"
        d.mkdir(exist_ok=True)
        self._write(d / asset_id, asset.data)
        self._write(d / f"{asset_id}.mime", asset.mime.encode())

    def get_asset(self, document_id: str, asset_id: str) -> Asset | None:
        if not ASSET_ID.match(asset_id):
            return None
        d = self._existing(document_id) / "assets"
        path = d / asset_id
        if not path.is_file():
            return None
        return Asset(path.read_bytes(), (d / f"{asset_id}.mime").read_text().strip())


def _dumps(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def _copy(sidecar: JSON | None) -> JSON | None:
    # Callers may mutate what they get; a JSON round trip is the cheapest deep copy that also
    # proves the sidecar is JSON (a FileStore would choke on anything else later).
    if sidecar is None:
        return None
    result: JSON = json.loads(json.dumps(sidecar))
    return result


def now() -> float:
    return time.time()
