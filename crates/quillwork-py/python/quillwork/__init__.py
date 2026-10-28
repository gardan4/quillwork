"""Quillwork: lossless, AI-friendly Word and PowerPoint editing.

Layers, bottom up:

- :class:`Package` - the .docx zip (OPC). Unedited parts round-trip byte for byte.
- :class:`XmlDocument` - one XML part, lossless; nodes are ``int`` handles.
- :class:`Document` - the op language (JSON per ``spec/schema``): transactions, save + sidecar.
- :class:`Presentation` - existing PPTX text, literal template fill and candidate review.

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
from .presentation import Presentation

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
    "Presentation",
    "ProtectedError",
    "QuillworkError",
    "Relationship",
    "UnsupportedError",
    "XmlDoctypeError",
    "XmlDocument",
    "XmlError",
    "XmlLimits",
    "XmlNodeError",
    "XmlParseError",
    "XmlValueError",
    "__version__",
    "errors",
    "ns",
]
