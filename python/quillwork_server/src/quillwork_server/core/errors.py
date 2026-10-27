"""Spec error objects (``spec/schema/error.schema.json``) built and rendered on the service side.

The service raises the binding's own exception classes (:mod:`quillwork.errors`), so a host sees
one error type whether the engine or the service refused a call. :func:`make_error` builds one
with a complete spec ``error`` object; :func:`envelope` turns any exception into the
``{spec_version, ok: false, error}`` response the front ends return.
"""

from __future__ import annotations

from typing import Any, Final

from quillwork.errors import QuillworkError, error_from_dict

from .types import JSON

SPEC_VERSION: Final = "0.1.0"

#: retryable per code (spec/errors.md). UNSUPPORTED depends on the reason; see make_error.
RETRYABLE: Final[dict[str, bool]] = {
    "STALE_ANCHOR": True,
    "AMBIGUOUS": True,
    "NOT_FOUND": True,
    "IN_FIELD": True,
    "INVALID_OP": True,
    "UNSUPPORTED": False,
    "CONFIRM_REQUIRED": True,
    "PROTECTED": False,
    "LIMIT": True,
    "BAD_VERSION": True,
    "CONFLICT": True,
    "BAD_PACKAGE": False,
    "INVARIANT": False,
    "INTERNAL": True,
}

#: UNSUPPORTED reasons a retry with other arguments can get past.
_RETRYABLE_UNSUPPORTED: Final = frozenset({"direct_only", "track_only"})


def make_error(
    code: str,
    message: str,
    *,
    version: int,
    op_index: int | None = None,
    retryable: bool | None = None,
    **fields: Any,
) -> QuillworkError:
    """The binding exception for ``code`` carrying a full spec error object.

    ``fields`` are the per-code members (``anchor``, ``current``, ``what``, ``reason``, ``limit``,
    ``path``, ``usage``, ``candidates``...); ``None`` values are left out.
    """
    if retryable is None:
        retryable = RETRYABLE.get(code, False)
        if code == "UNSUPPORTED":
            retryable = fields.get("reason") in _RETRYABLE_UNSUPPORTED
    error: JSON = {
        "code": code,
        "message": message,
        "retryable": retryable,
        "version": max(1, int(version)),
        "op_index": op_index,
    }
    for key, value in fields.items():
        if value is not None:
            error[key] = value
    return error_from_dict(error)


def error_object(exc: BaseException, *, version: int | None = None) -> JSON:
    """The spec ``error`` object for any exception.

    ``version`` fills in a missing current version (the binding leaves it ``None`` for errors raised
    before a document exists, e.g. BAD_PACKAGE on upload).
    """
    if isinstance(exc, QuillworkError):
        error = dict(exc.error)
        error.setdefault("code", exc.code)
        error.setdefault("message", exc.message or str(exc))
        error.setdefault("retryable", exc.retryable)
        error.setdefault("op_index", exc.op_index)
        if not isinstance(error.get("version"), int) or error["version"] < 1:
            error["version"] = max(1, version or exc.version or 1)
        if error["code"] in ("CONFLICT", "INTERNAL"):
            error["retryable"] = True
        if error["code"] in ("INVARIANT", "BAD_PACKAGE", "PROTECTED"):
            error["retryable"] = False
        return error
    return {
        "code": "INTERNAL",
        "message": "The engine failed unexpectedly; retry once, then report it.",
        "retryable": True,
        "version": max(1, version or 1),
        "op_index": None,
    }


def envelope(exc: BaseException, *, version: int | None = None) -> JSON:
    """``{spec_version, ok: false, error}`` for ``exc``."""
    return {"spec_version": SPEC_VERSION, "ok": False, "error": error_object(exc, version=version)}


def document_not_found(document_id: str) -> QuillworkError:
    # `what: "document"` is not in the 0.1.0 enum yet (proposed in docs/SERVER.md). `version` is
    # required (minimum 1) even though no document exists, so it is 1.
    return make_error(
        "NOT_FOUND", f"There is no document {document_id!r}.", version=1, what="document"
    )
