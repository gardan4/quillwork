"""The JSON op seam: ``Document.apply`` / ``read`` / ``find`` / ``view_model``.

Requests and results are plain dicts shaped by ``spec/schema`` (``ops.schema.json``,
``error.schema.json``, ``view-model.schema.json``). They cross into Rust as one JSON string each
way; Rust (serde) owns the shapes, so there is no per-field Python class to keep in sync. An
``ok: false`` envelope is raised as the matching :mod:`quillwork.errors` exception.

``apply`` runs the core transaction engine. ``read_op`` runs any read op of the catalogue
(spec/projection.md §8); ``read``, ``find``, ``get_block``, ``list_changes``, ``list_comments``,
``list_images``, ``get_image``, ``lint``, ``list_fields`` and ``help`` are typed shortcuts for
them. The text ops (``read``, ``find``, ``lint``, ``help``) return the result's ``text``; the
others return its ``data`` dict. ``view_model`` returns the view model; once it has been called,
every ``apply`` result's ``patch`` is the full patch from it.
"""

from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from typing import Any, Literal

from . import _native
from .errors import error_from_dict

__all__ = ["ChangeStatus", "Document", "JsonObject", "ReadMode"]

JsonObject = dict[str, Any]
ReadMode = Literal["accepted", "markup"]
ChangeStatus = Literal["open", "accepted", "rejected", "partial"]


def _dumps(value: JsonObject | str) -> str:
    return value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)


def _result(raw: str) -> JsonObject:
    out = json.loads(raw)
    if not isinstance(out, dict):
        raise TypeError("engine returned a non-object result")
    if out.get("ok") is False:
        error = out.get("error")
        raise error_from_dict(error if isinstance(error, dict) else {"code": "INTERNAL"})
    return out


class Document:
    """A .docx under edit, addressed through the op language.

    >>> doc = Document.open(data)                                   # doctest: +SKIP
    >>> doc.apply({"base_version": doc.version, "ops": [...]})      # doctest: +SKIP
    >>> data, sidecar = doc.save_with_sidecar()                     # doctest: +SKIP
    >>> doc = Document.open(data, sidecar=sidecar)  # same anchors, change sets and version

    One object is not thread-safe: serialise calls on it (a call that overlaps another raises
    ``RuntimeError: Already borrowed``).
    """

    __slots__ = ("_inner",)

    def __init__(self, inner: _native.JsonDocument) -> None:
        self._inner = inner

    @classmethod
    def open(
        cls,
        data: bytes | bytearray,
        limits: _native.Limits | None = None,
        *,
        sidecar: str | Mapping[str, Any] | None = None,
        version: int | None = None,
    ) -> Document:
        """Open a .docx.

        ``sidecar`` is what :meth:`sidecar` / :meth:`save_with_sidecar` returned for these bytes
        (or an earlier version of them): anchors, the change-set log and the version carry over.
        The document opens at ``max(version, the sidecar's version)``, and at version 1 when
        neither is given; a ``version`` below the sidecar's raises ``BadVersionError``.

        ``limits`` are checked on top of the engine's defaults. Raises ``BadPackageError`` (also
        for a sidecar that does not parse), ``EncryptedError``, ``LimitError``.
        """
        side = None if sidecar is None else _dumps(dict(sidecar)) if isinstance(sidecar, Mapping) else sidecar
        return cls(_native.JsonDocument.open(data, limits, sidecar=side, version=version))

    @property
    def version(self) -> int:
        """The current version; every committed transaction adds one."""
        return self._inner.version

    def apply(self, transaction: JsonObject | str) -> JsonObject:
        """Apply a Transaction atomically; returns the OpResult dict.

        A failed transaction changes nothing and raises the :mod:`quillwork.errors` subclass for
        its code, with the spec error object in ``.error``.
        """
        return _result(self._inner.apply_json(_dumps(transaction)))

    def apply_text(self, transaction: JsonObject | str) -> str:
        """Apply a Transaction; return the spec's text form (projection.md §9) of the result.

        Errors are rendered as text too, not raised.
        """
        return self._inner.apply_text(_dumps(transaction))

    def read_op(self, request: JsonObject | str) -> JsonObject:
        """Run a ReadOp (``{"op": "read", ...}``); returns the ReadResult dict."""
        return _result(self._inner.read_json(_dumps(request)))

    def read(
        self,
        selection: str | None = None,
        mode: ReadMode = "accepted",
        budget: int | None = None,
    ) -> str:
        """The projection text of ``selection`` (an anchor or ``"b3..b9"``; ``None`` = all)."""
        request: JsonObject = {"op": "read", "mode": mode}
        if selection is not None:
            request["range"] = selection
        if budget is not None:
            request["budget"] = budget
        text = self.read_op(request).get("text")
        return text if isinstance(text, str) else ""

    def find(
        self,
        query: str,
        *,
        regex: bool = False,
        story: str | None = None,
        mode: ReadMode = "accepted",
    ) -> str:
        """The ``§ find`` result text: matching blocks as projection lines."""
        request: JsonObject = {"op": "find", "query": query, "regex": regex, "mode": mode}
        if story is not None:
            request["story"] = story
        text = self.read_op(request).get("text")
        return text if isinstance(text, str) else ""

    def _text(self, request: JsonObject) -> str:
        text = self.read_op(request).get("text")
        return text if isinstance(text, str) else ""

    def _data(self, request: JsonObject) -> JsonObject:
        data = self.read_op(request).get("data")
        if not isinstance(data, dict):
            raise TypeError(f"engine returned no data for {request['op']}")
        return data

    def get_block(self, anchor: str) -> JsonObject:
        """``{"block": ...}``: one block as the view model holds it (paragraph, table or image)."""
        return self._data({"op": "get_block", "anchor": anchor})

    def list_changes(
        self,
        *,
        change_set: str | None = None,
        author: str | None = None,
        status: ChangeStatus | None = None,
    ) -> JsonObject:
        """``{"change_sets": [...], "revisions": [...]}``, filtered: the tracked changes in
        document order, each with its change set (``None`` when no agent transaction made it)."""
        request: JsonObject = {"op": "list_changes"}
        for key, value in (("change_set", change_set), ("author", author), ("status", status)):
            if value is not None:
                request[key] = value
        return self._data(request)

    def list_comments(self, *, open_only: bool = False) -> JsonObject:
        """``{"comments": [...]}``: comment threads in document order, replies nested in each."""
        request: JsonObject = {"op": "list_comments"}
        if open_only:
            request["open_only"] = True
        return self._data(request)

    def list_images(self) -> JsonObject:
        """``{"images": [...]}``: every picture with its anchor, asset id, size and alt text."""
        return self._data({"op": "list_images"})

    def get_image(self, image: str | None = None, *, asset: str | None = None) -> JsonObject:
        """One image's bytes (``base64``, ``mime``, ``px``), by image anchor or asset id."""
        if (image is None) == (asset is None):
            raise TypeError("get_image takes exactly one of image or asset")
        request: JsonObject = {"op": "get_image"}
        if image is not None:
            request["image"] = image
        else:
            request["asset"] = asset
        return self._data(request)

    def lint(self) -> str:
        """The document's ``§ lint:`` lines (``§ lint: none`` when clean)."""
        return self._text({"op": "lint"})

    def list_fields(self) -> JsonObject:
        """``{"controls": [...], "placeholders": [...]}``: content controls and blanks to fill."""
        return self._data({"op": "list_fields"})

    def help(self, name: str | None = None) -> str:
        """One-line signatures of every op, or of ``name``."""
        request: JsonObject = {"op": "help"}
        if name is not None:
            request["name"] = name
        return self._text(request)

    def view_model(self) -> JsonObject:
        """The view model (``view-model.schema.json``). After the first call the engine keeps it
        current, and every ``apply`` result carries the full patch from it."""
        return _result(self._inner.view_model_json())

    def save(self) -> bytes:
        """The document as .docx bytes. An unedited document saves byte-identical."""
        return self._inner.save()

    def sidecar(self) -> str:
        """The sidecar JSON for the document as it is now: pass it back to :meth:`open`."""
        return self._inner.sidecar()

    def save_with_sidecar(self) -> tuple[bytes, str]:
        """``(docx bytes, sidecar JSON)`` from one save."""
        return self._inner.save_with_sidecar()

    def change_sets(self) -> list[JsonObject]:
        """The change-set log (view-model ``ChangeSet`` objects), oldest first."""
        out = json.loads(self._inner.change_sets_json())
        if not isinstance(out, list):
            raise TypeError("engine returned a non-list change-set log")
        return out

    def render(self, handles: Sequence[str]) -> list[str]:
        """Markup-mode projection lines of the given anchors (unknown ones are skipped)."""
        return self._inner.render(list(handles))

    def set_agent(self, author: str) -> None:
        """The default revision author; a transaction's ``author`` overrides it."""
        self._inner.set_agent(author)

    def set_now(self, unix_secs: int | None = None) -> None:
        """Fix the clock revisions are stamped with (Unix seconds); ``None`` = system clock."""
        self._inner.set_now(unix_secs)

    def __repr__(self) -> str:
        return f"<quillwork.Document version={self.version}>"
