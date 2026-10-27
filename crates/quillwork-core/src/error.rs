//! Crate-wide error type. Modules add variants as they need them.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("limit exceeded: {0}")]
    Limit(String),
    #[error("invalid package: {0}")]
    Package(String),
    #[error("xml: {0}")]
    Xml(String),
    /// Password-protected / encrypted input (OLE `EncryptedPackage`, encrypted zip entries).
    #[error("encrypted: {0}")]
    Encrypted(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Other(String),
    /// Malformed XML. `offset` is a byte offset into the part.
    #[error("xml parse error at byte {offset}: {msg}")]
    XmlParse { offset: usize, msg: String },
    /// A `<!DOCTYPE` was found. OOXML never needs one; refusing it rules out DTDs,
    /// custom entities and entity-expansion bombs.
    #[error("xml: DOCTYPE declarations are not allowed (offset {offset})")]
    XmlDoctype { offset: usize },
    /// An XML tree operation was given a bad node id or would make the tree invalid.
    #[error("xml node: {0}")]
    XmlNode(String),
    /// A name or value that cannot be written as XML (bad qualified name, illegal character).
    #[error("xml value: {0}")]
    XmlValue(String),
    /// A styles, theme, numbering or settings part that cannot be used for style resolution.
    #[error("style: {0}")]
    Style(String),
    /// A Word-model operation failed (missing part, node outside a story, …).
    #[error("wml: {0}")]
    Wml(String),
    /// An anchor could not be resolved or the sidecar is malformed.
    #[error("anchor: {0}")]
    Anchor(String),
    /// An edit (`edit::review` / `comments` / `tables`) was refused. `code` is a spec error
    /// code: `NOT_FOUND`, `AMBIGUOUS`, `UNSUPPORTED`, `CONFIRM_REQUIRED`, `INVALID_OP`, `CONFLICT`.
    #[error("{code}: {message}")]
    Edit { code: &'static str, message: String },
    /// A request or safety limit was exceeded (`LIMIT` in spec/errors.md).
    #[error("limit {name}: {actual} > {max}")]
    LimitExceeded { name: String, max: i64, actual: i64 },
}

pub type Result<T> = std::result::Result<T, Error>;
