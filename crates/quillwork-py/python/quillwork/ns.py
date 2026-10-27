"""Namespace URIs (core ``xml::ns``) and relationship types.

Plain ``str`` constants so they type-check and autocomplete; ``tests/test_ns.py`` checks them
against the native table so they cannot drift from the Rust side.
"""

from typing import Final

W: Final = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
W14: Final = "http://schemas.microsoft.com/office/word/2010/wordml"
W15: Final = "http://schemas.microsoft.com/office/word/2012/wordml"
W16CID: Final = "http://schemas.microsoft.com/office/word/2016/wordml/cid"
W16CEX: Final = "http://schemas.microsoft.com/office/word/2018/wordml/cex"
R: Final = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
WP: Final = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
WP14: Final = "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing"
A: Final = "http://schemas.openxmlformats.org/drawingml/2006/main"
PIC: Final = "http://schemas.openxmlformats.org/drawingml/2006/picture"
MC: Final = "http://schemas.openxmlformats.org/markup-compatibility/2006"
V: Final = "urn:schemas-microsoft-com:vml"
O: Final = "urn:schemas-microsoft-com:office:office"
M: Final = "http://schemas.openxmlformats.org/officeDocument/2006/math"
XML: Final = "http://www.w3.org/XML/1998/namespace"
REL: Final = "http://schemas.openxmlformats.org/package/2006/relationships"
CT: Final = "http://schemas.openxmlformats.org/package/2006/content-types"
XMLNS: Final = "http://www.w3.org/2000/xmlns/"

#: Conventional prefix -> URI, in core order (``xml::ns::CONVENTIONAL``).
CONVENTIONAL: Final[dict[str, str]] = {
    "w": W,
    "w14": W14,
    "w15": W15,
    "w16cid": W16CID,
    "w16cex": W16CEX,
    "r": R,
    "wp": WP,
    "wp14": WP14,
    "a": A,
    "pic": PIC,
    "mc": MC,
    "v": V,
    "o": O,
    "m": M,
    "xml": XML,
}

#: Relationship type of the main document part (``_rels/.rels`` -> ``word/document.xml``).
RT_OFFICE_DOCUMENT: Final = R + "/officeDocument"
