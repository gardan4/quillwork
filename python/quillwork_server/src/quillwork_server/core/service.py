"""The document service: versions, one writer per document, and the engine behind them.

Hosts embed :class:`DocumentService` directly; the MCP server and the HTTP
service are thin front ends over it. Every method is synchronous and thread-safe; async hosts
call it from a worker thread.

How a write goes (:meth:`DocumentService.apply`):

1. take the document's writer lock (:class:`~.locks.LockProvider`; a timeout is CONFLICT);
2. get the live engine document at the head (cached; reopened from the store when the cache
   is cold or behind);
3. ``engine.apply(tx)`` - the engine checks ``base_version`` and every anchor guard, and is
   atomic: a refused transaction leaves the document as it was;
4. ``store.put_version(expected_head=head, ...)`` - compare-and-swap on the head, so a second
   writer that slipped past an advisory lock cannot overwrite a version (it gets CONFLICT
   after one retry against the new head);
5. announce the new head on the :class:`~.locks.Notifier`.
"""

from __future__ import annotations

import copy
import threading
import uuid
from collections import OrderedDict
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Any, Literal

from quillwork.errors import QuillworkError

from .engine import Engine, EngineDocument, asset_id, sniff_image
from .errors import document_not_found, make_error
from .locks import LocalLocks, LocalNotifier, LockProvider, LockTimeout, Notifier
from .store import (
    DocumentExists,
    DocumentNotFound,
    HeadMoved,
    MemoryStore,
    Store,
    VersionNotFound,
    check_document_id,
    now,
)
from .types import JSON, Asset, DocumentInfo, PatchFeed, StoredVersion, VersionKind, VersionRecord

ExportVariant = Literal["redline", "clean", "original"]


@dataclass(frozen=True, slots=True)
class ServiceConfig:
    #: Largest upload or import, in bytes.
    max_upload_bytes: int = 50 * 1024 * 1024
    #: Largest image a caller may upload as an asset, in bytes.
    max_asset_bytes: int = 20 * 1024 * 1024
    #: Ops per transaction; the service refuses more with LIMIT before the engine sees them.
    max_ops_per_transaction: int = 200
    #: Seconds to wait for a document's writer lock before answering CONFLICT.
    lock_timeout: float = 10.0
    #: Open engine documents kept in memory (least recently used are dropped).
    max_live_documents: int = 32
    #: Author of a transaction that names none.
    default_author: str = "AI"


class _Live:
    """A cached engine document and the mutex that serialises calls on it."""

    __slots__ = ("doc", "mutex", "version")

    def __init__(self, doc: EngineDocument, version: int) -> None:
        self.doc = doc
        self.version = version
        self.mutex = threading.Lock()


class DocumentService:
    def __init__(
        self,
        engine: Engine,
        store: Store | None = None,
        *,
        locks: LockProvider | None = None,
        notifier: Notifier | None = None,
        config: ServiceConfig | None = None,
        id_factory: Callable[[], str] | None = None,
    ) -> None:
        self.engine = engine
        self.store: Store = store if store is not None else MemoryStore()
        self.locks: LockProvider = locks if locks is not None else LocalLocks()
        self.notifier: Notifier = notifier if notifier is not None else LocalNotifier()
        self.config = config if config is not None else ServiceConfig()
        self._new_id = id_factory or (lambda: "d" + uuid.uuid4().hex[:16])
        self._cache: OrderedDict[str, _Live] = OrderedDict()
        self._cache_guard = threading.Lock()

    # -- documents ---------------------------------------------------------------------------

    def upload(
        self,
        data: bytes,
        *,
        name: str = "document.docx",
        document_id: str | None = None,
        author: str | None = None,
    ) -> DocumentInfo:
        """Store ``data`` as version 1 of a new document after the engine has opened it.

        Raises QuillworkError: LIMIT (too large), BAD_PACKAGE / PROTECTED (engine refused it),
        INVALID_OP (bad or taken document id).
        """
        self._check_size(data, self.config.max_upload_bytes, "uncompressed_bytes", 1)
        doc_id = document_id if document_id is not None else self._new_id()
        try:
            check_document_id(doc_id)
        except ValueError:
            raise _bad_id(doc_id) from None
        doc = self.engine.open(data, version=1)
        record = VersionRecord(doc_id, 1, None, "upload", now(), author=author, label=name)
        try:
            info = self.store.create(doc_id, name, data, doc.sidecar(), record)
        except DocumentExists:
            raise make_error(
                "INVALID_OP",
                f"There is already a document {doc_id!r}.",
                version=1,
                path="/document_id",
                usage="upload(data, name=, document_id=<new id>)",
            ) from None
        self._remember(doc_id, doc, 1)
        self.notifier.publish(doc_id, 1)
        return info

    def import_version(
        self,
        document_id: str,
        data: bytes,
        *,
        author: str | None = None,
        label: str | None = None,
    ) -> VersionRecord:
        """Make ``data`` (the document edited outside Quillwork, e.g. in Word) the next version.

        The engine reopens it with the previous sidecar so anchors carry over where it can
        match blocks. Viewers reload: the record has no patch.
        """
        self._check_size(data, self.config.max_upload_bytes, "uncompressed_bytes", 1)
        return self._snapshot(
            document_id, "import", lambda prev: (data, prev.sidecar), author=author, label=label
        )

    def restore(
        self,
        document_id: str,
        version: int,
        *,
        author: str | None = None,
        label: str | None = None,
    ) -> VersionRecord:
        """Make the snapshot of ``version`` the next version (undo for direct-mode edits).

        Direct-mode transactions leave no change set to reject, so DESIGN §11.1's "restore the
        previous snapshot" is this. History is kept: the restored state is a new version.
        """
        old = self._stored(document_id, version)
        return self._snapshot(
            document_id,
            "restore",
            lambda _prev: (old.data, old.sidecar),
            author=author,
            label=label or f"restore v{version}",
        )

    def _snapshot(
        self,
        document_id: str,
        kind: VersionKind,
        source: Callable[[StoredVersion], tuple[bytes, JSON | None]],
        *,
        author: str | None,
        label: str | None,
    ) -> VersionRecord:
        with self._writer(document_id):
            head = self._head(document_id)
            data, sidecar = source(self._stored(document_id, head))
            doc = self.engine.open(data, version=head + 1, sidecar=sidecar)
            record = VersionRecord(
                document_id, head + 1, head, kind, now(), author=author, label=label
            )
            self._commit(document_id, head, data, doc.sidecar(), record)
            self._remember(document_id, doc, head + 1)
        self.notifier.publish(document_id, head + 1)
        return record

    def info(self, document_id: str) -> DocumentInfo:
        try:
            return self.store.info(document_id)
        except DocumentNotFound:
            raise document_not_found(document_id) from None

    def list_documents(self) -> list[DocumentInfo]:
        return self.store.list_documents()

    def versions(self, document_id: str, after: int = 0) -> list[VersionRecord]:
        try:
            return self.store.records(document_id, after)
        except DocumentNotFound:
            raise document_not_found(document_id) from None

    def head(self, document_id: str) -> int:
        return self._head(document_id)

    def session(self, document_id: str) -> Session:
        """A handle on one document (raises NOT_FOUND when there is none)."""
        self._head(document_id)
        return Session(self, document_id)

    # -- reads -------------------------------------------------------------------------------

    def read_op(self, document_id: str, request: JSON, *, version: int | None = None) -> JSON:
        """Run a ReadOp (``{"op": "read" | "find" | "outline" | ..., ...}``) at ``version``."""
        with self._document(document_id, version) as doc:
            return doc.read_op(copy.deepcopy(request))

    def read(
        self,
        document_id: str,
        selection: str | None = None,
        *,
        mode: str = "accepted",
        budget: int | None = None,
        version: int | None = None,
    ) -> JSON:
        request: JSON = {"op": "read", "mode": mode}
        if selection:
            request["range"] = selection
        if budget is not None:
            request["budget"] = budget
        return self.read_op(document_id, request, version=version)

    def find(
        self,
        document_id: str,
        query: str,
        *,
        regex: bool = False,
        mode: str = "accepted",
        version: int | None = None,
    ) -> JSON:
        request: JSON = {"op": "find", "query": query, "regex": regex, "mode": mode}
        return self.read_op(document_id, request, version=version)

    def view_model(self, document_id: str, *, version: int | None = None) -> JSON:
        with self._document(document_id, version) as doc:
            return doc.view_model()

    def export(
        self,
        document_id: str,
        *,
        version: int | None = None,
        variant: ExportVariant = "redline",
    ) -> bytes:
        """The .docx bytes of ``version`` (default head).

        ``redline`` is the file as stored (tracked changes in it); ``clean`` accepts every
        change in a scratch copy; ``original`` is the upload, byte for byte.
        """
        if variant == "original":
            return self._stored(document_id, 1).data
        stored = self._stored(document_id, version or self._head(document_id))
        if variant == "redline":
            return stored.data
        if variant != "clean":
            raise make_error(
                "INVALID_OP",
                f"Unknown export variant {variant!r}.",
                version=stored.record.version,
                path="/variant",
                usage="redline|clean|original",
            )
        v = stored.record.version
        doc = self.engine.open(stored.data, version=v, sidecar=stored.sidecar)
        doc.apply({"base_version": v, "mode": "direct", "ops": [{"op": "accept", "all": True}]})
        return doc.save()

    def patches_since(self, document_id: str, since: int) -> PatchFeed:
        """The patches that take a viewer at ``since`` to the head.

        ``reload`` is set when one of the versions has no patch (upload, import) or ``since`` is
        not a version this document had.
        """
        head = self._head(document_id)
        if since < 1 or since > head:
            return PatchFeed(since, head, [], reload=True)
        # A writer may commit between these reads. Bound the feed to the sampled head so
        # its advertised version and its terminal patch agree, even across processes.
        records = [r for r in self.versions(document_id, after=since) if r.version <= head]
        patches: list[JSON] = []
        previous = since
        for record in records:
            patch = record.patch
            if (
                record.version != previous + 1
                or record.parent != previous
                or patch is None
                or patch.get("base_version") != previous
                or patch.get("version") != record.version
            ):
                return PatchFeed(since, head, [], reload=True)
            patches.append(patch)
            previous = record.version
        if previous != head:
            return PatchFeed(since, head, [], reload=True)
        return PatchFeed(since, head, patches)

    def wait(self, document_id: str, after: int, timeout: float) -> int | None:
        """Block until the head passes ``after`` (or ``timeout``); return the new head or None."""
        head = self._head(document_id)
        if head > after:
            return head
        return self.notifier.wait(document_id, after, timeout)

    # -- writes ------------------------------------------------------------------------------

    def apply(
        self,
        document_id: str,
        transaction: JSON,
        *,
        author: str | None = None,
        turn: str | None = None,
        kind: VersionKind = "apply",
    ) -> JSON:
        """Apply a Transaction; on success store the next version and return the OpResult.

        ``author`` and ``turn`` from the host win over those in the transaction (an agent should
        not be able to sign as someone else). Raises QuillworkError with the spec error.
        """
        tx = copy.deepcopy(transaction)
        if author is not None:
            tx["author"] = author
        tx.setdefault("author", self.config.default_author)
        if turn is not None:
            tx["turn"] = turn
        ops = tx.get("ops")
        if isinstance(ops, list) and len(ops) > self.config.max_ops_per_transaction:
            raise make_error(
                "LIMIT",
                f"A transaction may hold at most {self.config.max_ops_per_transaction} ops; "
                "split it.",
                version=self._head(document_id),
                limit={
                    "name": "ops_per_transaction",
                    "max": self.config.max_ops_per_transaction,
                    "actual": len(ops),
                },
            )
        for attempt in (1, 2):
            try:
                result = self._apply_once(document_id, tx, kind)
            except HeadMoved as moved:
                self._forget(document_id)
                if attempt == 2:
                    raise make_error(
                        "CONFLICT",
                        "Another writer committed to this document at the same time; "
                        "read it again and retry.",
                        version=moved.actual,
                    ) from None
                continue
            self.notifier.publish(document_id, int(result["version"]))
            return result
        raise AssertionError("unreachable")  # pragma: no cover

    def undo(
        self,
        document_id: str,
        change_set: str,
        *,
        author: str | None = None,
        turn: str | None = None,
    ) -> JSON:
        """Undo a change set by rejecting it, as a new version (history is never rewritten)."""
        head = self._head(document_id)
        tx: JSON = {
            "base_version": head,
            "ops": [{"op": "reject", "change_set": change_set}],
            "label": f"undo {change_set}",
        }
        return self.apply(document_id, tx, author=author, turn=turn, kind="undo")

    # -- assets ------------------------------------------------------------------------------

    def put_asset(self, document_id: str, data: bytes) -> str:
        """Store an image for a later insert_image/replace_image; return its asset id.

        The type is taken from the bytes, never from the caller.
        """
        head = self._head(document_id)
        self._check_size(data, self.config.max_asset_bytes, "image_bytes", head)
        mime = sniff_image(data)
        if mime is None:
            raise make_error(
                "INVALID_OP",
                "Assets must be PNG, JPEG, GIF, WebP or SVG images.",
                version=head,
                path="/data",
                usage="put_asset(document_id, <image bytes>)",
            )
        aid = asset_id(data)
        self.store.put_asset(document_id, aid, Asset(bytes(data), mime))
        return aid

    def get_asset(self, document_id: str, asset_id_: str, *, version: int | None = None) -> Asset:
        stored_asset = self.store.get_asset(document_id, asset_id_)
        if stored_asset is not None:
            return stored_asset
        stored = self._stored(document_id, version or self._head(document_id))
        found = self.engine.asset(stored.data, asset_id_)
        if found is None:
            raise make_error(
                "NOT_FOUND",
                f"There is no asset {asset_id_}.",
                version=stored.record.version,
                what="asset",
            )
        return found

    # -- internals ---------------------------------------------------------------------------

    def _apply_once(self, document_id: str, tx: JSON, kind: VersionKind) -> JSON:
        with self._writer(document_id):
            head = self._head(document_id)
            live = self._live_at_head(document_id, head)
            with live.mutex:
                if live.version != head:  # someone dropped/replaced it between the calls
                    raise HeadMoved(document_id, live.version, head)
                try:
                    # The real engine emits full patches only after this instance has a VM
                    # baseline. Persisted feeds must also work after restart, cache eviction,
                    # or a write made by a different process.
                    live.doc.view_model()
                    result = live.doc.apply(tx)
                except QuillworkError:
                    raise  # the engine's transactions are atomic: the live doc is still `head`
                except Exception as exc:
                    self._forget(document_id)
                    raise make_error(
                        "INTERNAL",
                        "The engine failed unexpectedly; retry once, then report it.",
                        version=head,
                    ) from exc
                try:
                    version = int(result["version"])
                    record = VersionRecord(
                        document_id,
                        version,
                        head,
                        kind,
                        now(),
                        change_set=result.get("change_set") or _undone(tx, kind),
                        author=tx.get("author"),
                        label=tx.get("label"),
                        turn=tx.get("turn"),
                        patch=result.get("patch"),
                    )
                    self._commit(document_id, head, live.doc.save(), live.doc.sidecar(), record)
                except BaseException:
                    self._forget(document_id)
                    raise
                live.version = version
        return result

    def _commit(
        self,
        document_id: str,
        head: int,
        data: bytes,
        sidecar: JSON | None,
        record: VersionRecord,
    ) -> None:
        try:
            self.store.put_version(document_id, head, data, sidecar, record)
        except DocumentNotFound:
            raise document_not_found(document_id) from None

    @contextmanager
    def _writer(self, document_id: str) -> Iterator[None]:
        try:
            with self.locks.lock(document_id, self.config.lock_timeout):
                yield
        except LockTimeout:
            raise make_error(
                "CONFLICT",
                "Another writer holds this document; retry in a moment.",
                version=self._head(document_id),
            ) from None

    @contextmanager
    def _document(self, document_id: str, version: int | None) -> Iterator[EngineDocument]:
        head = self._head(document_id)
        if version is None or version == head:
            live = self._live_at_head(document_id, head)
            with live.mutex:
                if live.version == head:
                    yield live.doc
                    return
            # A writer moved it on while we waited: read the version asked for from the store.
        stored = self._stored(document_id, version or head)
        yield self.engine.open(stored.data, version=stored.record.version, sidecar=stored.sidecar)

    def _live_at_head(self, document_id: str, head: int) -> _Live:
        with self._cache_guard:
            live = self._cache.get(document_id)
            if live is not None and live.version == head:
                self._cache.move_to_end(document_id)
                return live
        stored = self._stored(document_id, head)
        doc = self.engine.open(stored.data, version=head, sidecar=stored.sidecar)
        return self._remember(document_id, doc, head)

    def _remember(self, document_id: str, doc: EngineDocument, version: int) -> _Live:
        with self._cache_guard:
            live = self._cache.get(document_id)
            if live is not None and live.version >= version:
                self._cache.move_to_end(document_id)
                return live
            live = _Live(doc, version)
            self._cache[document_id] = live
            self._cache.move_to_end(document_id)
            while len(self._cache) > max(1, self.config.max_live_documents):
                self._cache.popitem(last=False)
            return live

    def _forget(self, document_id: str) -> None:
        with self._cache_guard:
            self._cache.pop(document_id, None)

    def _head(self, document_id: str) -> int:
        try:
            return self.store.head(document_id)
        except DocumentNotFound:
            raise document_not_found(document_id) from None

    def _stored(self, document_id: str, version: int) -> StoredVersion:
        try:
            return self.store.get_version(document_id, version)
        except DocumentNotFound:
            raise document_not_found(document_id) from None
        except VersionNotFound as exc:
            raise make_error(
                "BAD_VERSION",
                f"Version {version} does not exist (the document is at v{exc.head}).",
                version=exc.head,
            ) from None

    @staticmethod
    def _check_size(data: bytes, limit: int, name: str, version: int) -> None:
        if len(data) > limit:
            raise make_error(
                "LIMIT",
                f"The file is {len(data)} bytes; the limit is {limit}.",
                version=version,
                limit={"name": name, "max": limit, "actual": len(data)},
            )


def _undone(tx: JSON, kind: VersionKind) -> str | None:
    if kind != "undo":
        return None
    ops = tx.get("ops") or []
    cs = ops[0].get("change_set") if ops and isinstance(ops[0], dict) else None
    return cs if isinstance(cs, str) else None


def _bad_id(document_id: str) -> QuillworkError:
    return make_error(
        "INVALID_OP",
        f"{document_id!r} is not a valid document id.",
        version=1,
        path="/document_id",
        usage="1-128 of A-Z a-z 0-9 . _ -, starting with a letter or digit",
    )


class Session:
    """One document, bound: the calls an agent turn makes, without repeating the id.

    ``version`` is the newest version this session has seen (from a read or its own write);
    :meth:`edit` uses it as ``base_version`` when the caller gives none.
    """

    def __init__(self, service: DocumentService, document_id: str) -> None:
        self.service = service
        self.document_id = document_id
        self.version = service.head(document_id)

    def _saw(self, result: JSON) -> JSON:
        v = result.get("version")
        if isinstance(v, int) and v > self.version:
            self.version = v
        return result

    def read(self, selection: str | None = None, **kwargs: Any) -> JSON:
        return self._saw(self.service.read(self.document_id, selection, **kwargs))

    def find(self, query: str, **kwargs: Any) -> JSON:
        return self._saw(self.service.find(self.document_id, query, **kwargs))

    def read_op(self, request: JSON) -> JSON:
        return self._saw(self.service.read_op(self.document_id, request))

    def apply(self, transaction: JSON, **kwargs: Any) -> JSON:
        return self._saw(self.service.apply(self.document_id, transaction, **kwargs))

    def edit(self, *ops: JSON, label: str | None = None, base_version: int | None = None) -> JSON:
        tx: JSON = {"base_version": base_version or self.version, "ops": list(ops)}
        if label:
            tx["label"] = label
        return self.apply(tx)

    def undo(self, change_set: str) -> JSON:
        return self._saw(self.service.undo(self.document_id, change_set))

    def restore(self, version: int) -> VersionRecord:
        record = self.service.restore(self.document_id, version)
        self.version = max(self.version, record.version)
        return record

    def view_model(self) -> JSON:
        return self.service.view_model(self.document_id)

    def export(self, variant: ExportVariant = "redline") -> bytes:
        return self.service.export(self.document_id, variant=variant)

    def patches_since(self, since: int | None = None) -> PatchFeed:
        return self.service.patches_since(
            self.document_id, self.version if since is None else since
        )
