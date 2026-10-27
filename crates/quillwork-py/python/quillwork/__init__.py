"""Quillwork: lossless, AI-friendly editing of Word (.docx) documents.

Layers, bottom up:

- :class:`Package` - the .docx zip (OPC). Unedited parts round-trip byte for byte.
- :class:`XmlDocument` - one XML part, lossless; nodes are ``int`` handles.
- :class:`Document` - the op language (JSON per ``spec/schema``): transactions, save + sidecar.

Errors are :class:`QuillworkError` subclasses (:mod:`quillwork.errors`).
"""

from . import errors, ns
from ._native import (
    SPEC_VERSION,
    Limits,
    Package,
    Relationship,
    XmlDocument,
    XmlLimits,
    __version__,
)
from .document import Document
from .errors import (
    BadPackageError,
    EncryptedError,
    InvalidOpError,
    LimitError,
    NotFoundError,
    ProtectedError,
    QuillworkError,
    UnsupportedError,
    XmlDoctypeError,
    XmlError,
    XmlNodeError,
    XmlParseError,
    XmlValueError,
)

__all__ = [
    "SPEC_VERSION",
    "BadPackageError",
    "Document",
    "EncryptedError",
    "InvalidOpError",
    "LimitError",
    "Limits",
    "NotFoundError",
    "Package",
    "ProtectedError",
    "QuillworkError",
    "Relationship",
    "UnsupportedError",
    "XmlDocument",
    "XmlDoctypeError",
    "XmlError",
    "XmlLimits",
    "XmlNodeError",
    "XmlParseError",
    "XmlValueError",
    "__version__",
    "errors",
    "ns",
]
