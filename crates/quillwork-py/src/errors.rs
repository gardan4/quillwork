//! Core `Error` → Python exception. The exception classes live in Python (`quillwork.errors`) so
//! they can carry typed attributes and inherit from builtins (`LookupError`, `ValueError`); this
//! module only decides which class and which attributes.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use quillwork_core::Error;

/// How a core error surfaces: the `quillwork.errors` class plus the structured attributes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Mapped {
    pub class: &'static str,
    pub code: &'static str,
    pub message: String,
    pub reason: Option<&'static str>,
    pub offset: Option<usize>,
    /// `(name, max, actual)` for a structured limit.
    pub limit: Option<(String, i64, i64)>,
}

/// The `quillwork.errors` class for a spec error code.
fn class_of(code: &str) -> &'static str {
    match code {
        "STALE_ANCHOR" => "StaleAnchorError",
        "AMBIGUOUS" => "AmbiguousError",
        "NOT_FOUND" => "NotFoundError",
        "IN_FIELD" => "InFieldError",
        "INVALID_OP" => "InvalidOpError",
        "UNSUPPORTED" => "UnsupportedError",
        "CONFIRM_REQUIRED" => "ConfirmRequiredError",
        "PROTECTED" => "ProtectedError",
        "LIMIT" => "LimitError",
        "BAD_VERSION" => "BadVersionError",
        "CONFLICT" => "ConflictError",
        "BAD_PACKAGE" => "BadPackageError",
        "INVARIANT" => "InvariantError",
        _ => "InternalError",
    }
}

/// The one mapping table. A core variant added later lands in the wildcard arm as INTERNAL until
/// it is mapped here.
pub(crate) fn classify(e: &Error) -> Mapped {
    let message = e.to_string();
    let m = |class, code| Mapped { class, code, message: message.clone(), reason: None, offset: None, limit: None };
    #[allow(unreachable_patterns)]
    match e {
        Error::Limit(_) => m("LimitError", "LIMIT"),
        Error::Package(_) => m("BadPackageError", "BAD_PACKAGE"),
        Error::Xml(_) => m("XmlError", "BAD_PACKAGE"),
        Error::XmlParse { offset, .. } => Mapped { offset: Some(*offset), ..m("XmlParseError", "BAD_PACKAGE") },
        Error::XmlDoctype { offset } => Mapped { offset: Some(*offset), ..m("XmlDoctypeError", "BAD_PACKAGE") },
        Error::Encrypted(_) => Mapped { reason: Some("encrypted"), ..m("EncryptedError", "PROTECTED") },
        Error::NotFound(_) => m("NotFoundError", "NOT_FOUND"),
        Error::XmlNode(_) => m("XmlNodeError", "INVALID_OP"),
        Error::XmlValue(_) => m("XmlValueError", "INVALID_OP"),
        // Malformed document content, or a sidecar that does not fit it (as Engine::open_error).
        Error::Style(_) | Error::Wml(_) | Error::Anchor(_) => m("BadPackageError", "BAD_PACKAGE"),
        Error::LimitExceeded { name, max, actual } => {
            Mapped { limit: Some((name.clone(), *max, *actual)), ..m("LimitError", "LIMIT") }
        }
        Error::Edit { code, .. } => {
            let code: &'static str = if class_of(code) == "InternalError" { "INTERNAL" } else { code };
            m(class_of(code), code)
        }
        Error::Other(_) => m("InternalError", "INTERNAL"),
        _ => m("InternalError", "INTERNAL"),
    }
}

/// Build the Python exception for `e`. Falls back to `RuntimeError` if `quillwork.errors` cannot
/// be imported (which only happens when `_native` is loaded outside its package).
pub(crate) fn to_py(e: Error) -> PyErr {
    let mapped = classify(&e);
    Python::attach(|py| build(py, &mapped).unwrap_or_else(|_| PyRuntimeError::new_err(mapped.message.clone())))
}

fn build(py: Python<'_>, m: &Mapped) -> PyResult<PyErr> {
    let module = py.import("quillwork.errors")?;
    let class = module.getattr(m.class)?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("code", m.code)?;
    if let Some(reason) = m.reason {
        kwargs.set_item("reason", reason)?;
    }
    if let Some(offset) = m.offset {
        kwargs.set_item("offset", offset)?;
    }
    if let Some((name, max, actual)) = &m.limit {
        let limit = PyDict::new(py);
        limit.set_item("name", name)?;
        limit.set_item("max", max)?;
        limit.set_item("actual", actual)?;
        kwargs.set_item("limit", limit)?;
    }
    let exc = class.call((m.message.as_str(),), Some(&kwargs))?;
    Ok(PyErr::from_value(exc))
}

/// `Result` adapter: `core_call().map_err(to_py)?` reads as `core_call().py()?`.
pub(crate) trait IntoPyResult<T> {
    fn py(self) -> PyResult<T>;
}

impl<T> IntoPyResult<T> for quillwork_core::Result<T> {
    fn py(self) -> PyResult<T> {
        self.map_err(to_py)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_has_a_class_and_spec_code() {
        let cases = [
            (Error::Limit("x".into()), "LimitError", "LIMIT"),
            (Error::Package("x".into()), "BadPackageError", "BAD_PACKAGE"),
            (Error::Xml("x".into()), "XmlError", "BAD_PACKAGE"),
            (Error::XmlParse { offset: 7, msg: "x".into() }, "XmlParseError", "BAD_PACKAGE"),
            (Error::XmlDoctype { offset: 3 }, "XmlDoctypeError", "BAD_PACKAGE"),
            (Error::Encrypted("x".into()), "EncryptedError", "PROTECTED"),
            (Error::NotFound("x".into()), "NotFoundError", "NOT_FOUND"),
            (Error::XmlNode("x".into()), "XmlNodeError", "INVALID_OP"),
            (Error::XmlValue("x".into()), "XmlValueError", "INVALID_OP"),
            (Error::Other("x".into()), "InternalError", "INTERNAL"),
            (Error::Style("x".into()), "BadPackageError", "BAD_PACKAGE"),
            (Error::Wml("x".into()), "BadPackageError", "BAD_PACKAGE"),
            (Error::Anchor("x".into()), "BadPackageError", "BAD_PACKAGE"),
            (Error::LimitExceeded { name: "n".into(), max: 1, actual: 2 }, "LimitError", "LIMIT"),
            (Error::Edit { code: "BAD_VERSION", message: "x".into() }, "BadVersionError", "BAD_VERSION"),
            (Error::Edit { code: "STALE_ANCHOR", message: "x".into() }, "StaleAnchorError", "STALE_ANCHOR"),
            (Error::Edit { code: "WHATEVER", message: "x".into() }, "InternalError", "INTERNAL"),
        ];
        for (e, class, code) in cases {
            let m = classify(&e);
            assert_eq!((m.class, m.code), (class, code), "{e:?}");
        }
        assert_eq!(classify(&Error::XmlParse { offset: 7, msg: "x".into() }).offset, Some(7));
        assert_eq!(classify(&Error::Encrypted("x".into())).reason, Some("encrypted"));
        let limit = classify(&Error::LimitExceeded { name: "n".into(), max: 1, actual: 2 }).limit;
        assert_eq!(limit, Some(("n".to_owned(), 1, 2)));
    }
}
