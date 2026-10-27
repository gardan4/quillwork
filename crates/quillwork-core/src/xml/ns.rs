//! Namespace URIs used across OOXML packages, plus their conventional prefixes.
//!
//! Match elements by URI (`Document::is(node, ns::W, "p")`), never by prefix: a producer may
//! bind `w` to anything, and non-Word producers sometimes use other prefixes.

/// WordprocessingML main (`w:`).
pub const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
/// Word 2010 extensions (`w14:`): `paraId`, `textId`, …
pub const W14: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
/// Word 2012 extensions (`w15:`): comment threading, …
pub const W15: &str = "http://schemas.microsoft.com/office/word/2012/wordml";
/// Word 2016 comment ids (`w16cid:`).
pub const W16CID: &str = "http://schemas.microsoft.com/office/word/2016/wordml/cid";
/// Word 2018 comment extensions (`w16cex:`).
pub const W16CEX: &str = "http://schemas.microsoft.com/office/word/2018/wordml/cex";
/// Office document relationships (`r:`), e.g. `r:id`, `r:embed`.
pub const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
/// DrawingML in Word (`wp:`): `wp:inline`, `wp:anchor`.
pub const WP: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
/// Word 2010 drawing extensions (`wp14:`).
pub const WP14: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing";
/// DrawingML main (`a:`).
pub const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
/// DrawingML pictures (`pic:`).
pub const PIC: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
/// Markup compatibility (`mc:`): `mc:AlternateContent`, `mc:Ignorable`.
pub const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
/// VML (`v:`).
pub const V: &str = "urn:schemas-microsoft-com:vml";
/// Office VML extensions (`o:`).
pub const O: &str = "urn:schemas-microsoft-com:office:office";
/// Office Math (`m:`).
pub const M: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";
/// Package relationships (`.rels` parts; default namespace there).
pub const REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
/// `[Content_Types].xml` (default namespace there).
pub const CT: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
/// The `xml:` prefix (bound implicitly), e.g. `xml:space`.
pub const XML: &str = "http://www.w3.org/XML/1998/namespace";
/// The namespace of `xmlns` / `xmlns:*` declarations themselves.
pub const XMLNS: &str = "http://www.w3.org/2000/xmlns/";

/// Conventional prefix for each well-known namespace, in the order above.
pub const CONVENTIONAL: &[(&str, &str)] = &[
    ("w", W),
    ("w14", W14),
    ("w15", W15),
    ("w16cid", W16CID),
    ("w16cex", W16CEX),
    ("r", R),
    ("wp", WP),
    ("wp14", WP14),
    ("a", A),
    ("pic", PIC),
    ("mc", MC),
    ("v", V),
    ("o", O),
    ("m", M),
    ("xml", XML),
];

/// The prefix Word conventionally uses for `uri`, if it is a well-known namespace.
pub fn conventional_prefix(uri: &str) -> Option<&'static str> {
    CONVENTIONAL.iter().find(|(_, u)| *u == uri).map(|(p, _)| *p)
}
