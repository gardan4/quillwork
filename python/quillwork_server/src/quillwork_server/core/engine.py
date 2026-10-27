"""The engine seam: what the service needs from a document engine.

:class:`QuillworkEngine` adapts the real binding (``quillwork.Document``); ``fake.FakeEngine``
implements the same protocol on plain text for tests. Both speak the spec's JSON shapes: a
Transaction in, an OpResult out; a ReadOp in, a ReadResult out; errors as
:class:`quillwork.errors.QuillworkError`.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any, Protocol, runtime_checkable

from .types import JSON, Asset


@runtime_checkable
class EngineDocument(Protocol):
    """One open document. Not thread-safe: the service serialises calls on it."""

    @property
    def version(self) -> int: ...

    def apply(self, transaction: JSON) -> JSON:
        """Apply a Transaction atomically; return the OpResult. Raises QuillworkError."""
        ...

    def read_op(self, request: JSON) -> JSON:
        """Run a ReadOp; return the ReadResult. Raises QuillworkError."""
        ...

    def view_model(self) -> JSON: ...

    def save(self) -> bytes: ...

    def sidecar(self) -> JSON | None:
        """Engine state that is not in the .docx (anchor map, change sets), versioned with it."""
        ...


@runtime_checkable
class Engine(Protocol):
    name: str

    def open(self, data: bytes, *, version: int = 1, sidecar: JSON | None = None) -> EngineDocument:
        """Open ``data`` as ``version``, restoring ``sidecar`` when given.

        Raises QuillworkError (BAD_PACKAGE, PROTECTED, LIMIT) for unusable input.
        """
        ...

    def asset(self, data: bytes, asset_id: str) -> Asset | None:
        """The bytes of an image the document at ``data`` holds, by asset id (``a_<16 hex>``)."""
        ...


def asset_id(data: bytes) -> str:
    """Asset ids are the first 16 hex digits of sha256(bytes) (spec/anchors.md)."""
    return "a_" + hashlib.sha256(data).hexdigest()[:16]


_MAGIC: tuple[tuple[bytes, str], ...] = (
    (b"\x89PNG\r\n\x1a\n", "image/png"),
    (b"\xff\xd8\xff", "image/jpeg"),
    (b"GIF87a", "image/gif"),
    (b"GIF89a", "image/gif"),
)


def sniff_image(data: bytes) -> str | None:
    """The image MIME type from magic bytes, or ``None``. Never trusts a declared type."""
    for magic, mime in _MAGIC:
        if data.startswith(magic):
            return mime
    if data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return "image/webp"
    head = data[:512].lstrip().lower()
    if head.startswith(b"<svg") or (head.startswith(b"<?xml") and b"<svg" in head):
        return "image/svg+xml"
    return None


# -- the real binding ------------------------------------------------------------------------


class QuillworkEngine:
    """:class:`Engine` over :class:`quillwork.Document`.

    Versions and sidecars pass straight through: ``Document.open(data, sidecar=..., version=...)``
    opens at ``max(version, the sidecar's)``, which is how the service reopens an import or a
    restore at ``head + 1`` with an older sidecar.
    """

    name = "quillwork"

    def __init__(self, limits: Any | None = None) -> None:
        import quillwork

        self._quillwork = quillwork
        self._limits = limits

    def open(self, data: bytes, *, version: int = 1, sidecar: JSON | None = None) -> EngineDocument:
        doc = self._quillwork.Document.open(data, self._limits, sidecar=sidecar, version=version)
        return _QuillworkDocument(doc)

    def asset(self, data: bytes, asset_id_: str) -> Asset | None:
        package = self._quillwork.Package.open(data)
        for name in package.names():
            if not name.startswith("word/media/"):
                continue
            part = bytes(package.part(name))
            if asset_id(part) == asset_id_:
                mime = (
                    sniff_image(part) or package.content_type_of(name) or "application/octet-stream"
                )
                return Asset(part, mime)
        return None


class _QuillworkDocument:
    """:class:`EngineDocument` over an open :class:`quillwork.Document`."""

    def __init__(self, doc: Any) -> None:
        self._doc = doc

    @property
    def version(self) -> int:
        return int(self._doc.version)

    def apply(self, transaction: JSON) -> JSON:
        result: JSON = self._doc.apply(transaction)
        return result

    def read_op(self, request: JSON) -> JSON:
        result: JSON = self._doc.read_op(request)
        return result

    def view_model(self) -> JSON:
        result: JSON = self._doc.view_model()
        return result

    def save(self) -> bytes:
        return bytes(self._doc.save())

    def sidecar(self) -> JSON | None:
        """The sidecar as a JSON object (the binding caches it from the last save)."""
        result: JSON = json.loads(self._doc.sidecar())
        return result
