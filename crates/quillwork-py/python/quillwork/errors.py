"""Quillwork exceptions.

One base, :class:`QuillworkError`, and one subclass per spec error code (``spec/errors.md``).
Native calls raise these directly (the Rust side maps every core ``Error`` variant in one place);
JSON error envelopes from :class:`quillwork.Document` become them through :func:`error_from_dict`.

Every exception carries the spec fields as attributes (``code``, ``message``, ``retryable``,
``reason``, ``version``, ``op_index``, ...). Fields the native layer cannot fill yet are ``None``.
"""

from __future__ import annotations

from typing import Any, ClassVar, Final

__all__ = [
    "AmbiguousError",
    "BadPackageError",
    "BadVersionError",
    "ConfirmRequiredError",
    "ConflictError",
    "EncryptedError",
    "InFieldError",
    "InternalError",
    "InvalidOpError",
    "InvariantError",
    "LimitError",
    "NotFoundError",
    "ProtectedError",
    "QuillworkError",
    "StaleAnchorError",
    "UnsupportedError",
    "XmlDoctypeError",
    "XmlError",
    "XmlNodeError",
    "XmlParseError",
    "XmlValueError",
    "error_from_dict",
]


class QuillworkError(Exception):
    """Base of every Quillwork error.

    Attributes mirror ``spec/schema/error.schema.json``; ``error`` holds the full error object
    when the exception came from a JSON envelope (else a dict built from the attributes).
    """

    #: Spec code this class stands for; instances may carry a different ``code`` only when a
    #: caller constructs ``QuillworkError(code=...)`` directly.
    default_code: ClassVar[str] = "INTERNAL"
    default_retryable: ClassVar[bool] = False

    code: str
    message: str
    retryable: bool
    reason: str | None
    version: int | None
    op_index: int | None
    op: str | None
    anchor: str | None
    #: Byte offset into the XML part, for parse errors.
    offset: int | None
    #: ``{"name", "max", "actual"}`` for LIMIT, when the engine reports it.
    limit: dict[str, Any] | None
    error: dict[str, Any]

    def __init__(
        self,
        message: str = "",
        *,
        code: str | None = None,
        retryable: bool | None = None,
        reason: str | None = None,
        version: int | None = None,
        op_index: int | None = None,
        op: str | None = None,
        anchor: str | None = None,
        offset: int | None = None,
        limit: dict[str, Any] | None = None,
        error: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(message)
        self.message = message
        self.code = code if code is not None else self.default_code
        self.retryable = retryable if retryable is not None else self.default_retryable
        self.reason = reason
        self.version = version
        self.op_index = op_index
        self.op = op
        self.anchor = anchor
        self.offset = offset
        self.limit = limit
        self.error = error if error is not None else self._as_error()

    def _as_error(self) -> dict[str, Any]:
        out: dict[str, Any] = {
            "code": self.code,
            "message": self.message,
            "retryable": self.retryable,
            "version": self.version,
            "op_index": self.op_index,
        }
        for key in ("op", "anchor", "reason", "limit"):
            value = getattr(self, key)
            if value is not None:
                out[key] = value
        return out

    def get(self, field: str, default: Any = None) -> Any:
        """Any field of the error object, including per-code ones (``candidates``, ``path``...)."""
        return self.error.get(field, default)

    def __repr__(self) -> str:
        return f"{type(self).__name__}({self.message!r}, code={self.code!r})"


class StaleAnchorError(QuillworkError):
    default_code = "STALE_ANCHOR"
    default_retryable = True


class AmbiguousError(QuillworkError):
    default_code = "AMBIGUOUS"
    default_retryable = True


class NotFoundError(QuillworkError, LookupError):
    """NOT_FOUND. Natively: a missing part."""

    default_code = "NOT_FOUND"
    default_retryable = True


class InFieldError(QuillworkError):
    default_code = "IN_FIELD"
    default_retryable = True


class InvalidOpError(QuillworkError, ValueError):
    default_code = "INVALID_OP"
    default_retryable = True


class XmlNodeError(InvalidOpError):
    """A bad node id, or an edit that would make the XML tree invalid."""


class XmlValueError(InvalidOpError):
    """A name or value that cannot be written as XML."""


class UnsupportedError(QuillworkError):
    default_code = "UNSUPPORTED"


class ConfirmRequiredError(QuillworkError):
    default_code = "CONFIRM_REQUIRED"
    default_retryable = True


class ProtectedError(QuillworkError):
    default_code = "PROTECTED"


class EncryptedError(ProtectedError):
    """Password-protected input (OLE ``EncryptedPackage`` or encrypted zip entries).

    ``reason`` is ``"encrypted"``.
    """


class LimitError(QuillworkError):
    """A size, ratio, count or depth limit was exceeded (zip bombs land here).

    ``limit`` is ``None`` until the core reports structured limits.
    """

    default_code = "LIMIT"
    default_retryable = True


class BadVersionError(QuillworkError):
    default_code = "BAD_VERSION"
    default_retryable = True


class ConflictError(QuillworkError):
    default_code = "CONFLICT"
    default_retryable = True


class BadPackageError(QuillworkError, ValueError):
    """The input is not a usable .docx: not a zip, corrupt, or malformed XML inside."""

    default_code = "BAD_PACKAGE"


class XmlError(BadPackageError):
    pass


class XmlParseError(XmlError):
    """Malformed XML; ``offset`` is the byte offset into the part."""


class XmlDoctypeError(XmlParseError):
    """The XML has a ``<!DOCTYPE``; Quillwork refuses DTDs outright."""


class InvariantError(QuillworkError):
    default_code = "INVARIANT"


class InternalError(QuillworkError):
    default_code = "INTERNAL"
    default_retryable = True


#: Spec code -> exception class, for JSON envelopes.
BY_CODE: Final[dict[str, type[QuillworkError]]] = {
    cls.default_code: cls
    for cls in (
        StaleAnchorError,
        AmbiguousError,
        NotFoundError,
        InFieldError,
        InvalidOpError,
        UnsupportedError,
        ConfirmRequiredError,
        ProtectedError,
        LimitError,
        BadVersionError,
        ConflictError,
        BadPackageError,
        InvariantError,
        InternalError,
    )
}


def error_from_dict(error: dict[str, Any]) -> QuillworkError:
    """The exception for a spec error object (the ``error`` member of an ``ok: false`` envelope)."""
    code = str(error.get("code", "INTERNAL"))
    cls: type[QuillworkError] = BY_CODE.get(code, QuillworkError)
    if cls is ProtectedError and error.get("reason") == "encrypted":
        cls = EncryptedError

    def opt_int(key: str) -> int | None:
        value = error.get(key)
        return value if isinstance(value, int) else None

    def opt_str(key: str) -> str | None:
        value = error.get(key)
        return value if isinstance(value, str) else None

    limit = error.get("limit")
    return cls(
        str(error.get("message", "")),
        code=code,
        retryable=bool(error.get("retryable", cls.default_retryable)),
        reason=opt_str("reason"),
        version=opt_int("version"),
        op_index=opt_int("op_index"),
        op=opt_str("op"),
        anchor=opt_str("anchor"),
        limit=limit if isinstance(limit, dict) else None,
        error=dict(error),
    )
