//! The JSON op seam over the core engine ([`quillwork_core::engine::Engine`]). Requests and
//! results cross the boundary as JSON strings shaped by `spec/schema` (ops, results, errors, view
//! model); the Python wrapper (`quillwork.Document`) does `json.dumps` / `json.loads` and raises
//! an `ok: false` envelope as the matching `quillwork.errors` exception.
//!
//! - `apply_json` / `apply_text` run a Transaction through the engine (GIL released).
//! - `save` / `sidecar` persist the document and the engine state that is not in the .docx
//!   (anchors, change sets, version); `open(..., sidecar=...)` restores them.
//! - `read_json` runs any read op of the catalogue (spec/projection.md §8): the projection
//!   (`outline`, `read`, `find`), `get_block`, the review lists (`list_changes`,
//!   `list_comments`), the images (`list_images`, `get_image`), `lint`, `list_fields` and
//!   `help`. `view_model_json` returns the view model (GIL released for both).

use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;
use pyo3::types::PyBytes;
use quillwork_core::anchors::{self, Sidecar};
use quillwork_core::engine::Engine;
use quillwork_core::{Error, opc};
use serde_json::{Value, json};

use crate::errors::IntoPyResult;
use crate::package::Limits;

pub use quillwork_core::engine::SPEC_VERSION;

/// `format` of the engine sidecar (`{format, anchors, engine}`), which the core keeps private.
/// `open_reaches_the_requested_version` fails if it drifts.
const ENGINE_SIDECAR_FORMAT: u32 = 1;

/// The version a fresh upload gets (spec/schema/common.schema.json `Version`).
const FIRST_VERSION: u64 = 1;

/// The sidecar to hand [`Engine::open`] so the document opens at `max(version, sidecar's)`, or
/// at [`FIRST_VERSION`] when neither says. A requested version below the sidecar's is an error.
///
/// The engine has no version setter: it fast-forwards to the version its sidecar records, so a
/// higher version is written into the sidecar (wrapping a bare anchors sidecar, or an empty one
/// when there is none, which numbers the document exactly as a fresh mint does).
fn sidecar_at(sidecar: Option<&str>, version: Option<u64>) -> Result<Option<String>, Error> {
    let bad = |e: serde_json::Error| Error::Anchor(format!("sidecar: {e}"));
    let mut file = match sidecar {
        None => None,
        Some(s) => Some(serde_json::from_str::<Value>(s).map_err(bad)?),
    };
    let full = file.as_ref().is_some_and(|v| v.get("anchors").is_some());
    let have = if full {
        file.as_ref().and_then(|v| v.pointer("/engine/version")).and_then(Value::as_u64).unwrap_or(0)
    } else {
        0
    };
    if let Some(v) = version
        && v < have
    {
        return Err(Error::Edit {
            code: "BAD_VERSION",
            message: format!("version {v} is behind the sidecar's version {have}"),
        });
    }
    let want = version.unwrap_or(FIRST_VERSION).max(have).max(FIRST_VERSION);
    if want == have {
        return Ok(sidecar.map(str::to_owned));
    }
    let doc = match file.take() {
        Some(mut f) if full => {
            f["engine"]["version"] = json!(want);
            f
        }
        bare => {
            let anchors = match bare {
                Some(a) => a,
                None => serde_json::to_value(Sidecar { format: anchors::SIDECAR_FORMAT, ..Default::default() })
                    .map_err(bad)?,
            };
            json!({
                "format": ENGINE_SIDECAR_FORMAT,
                "anchors": anchors,
                "engine": {"version": want, "next_cs": 0, "change_sets": [], "last_changed": {}},
            })
        }
    };
    Ok(Some(doc.to_string()))
}

fn open_engine(
    data: Vec<u8>,
    limits: Option<opc::Limits>,
    sidecar: Option<&str>,
    version: Option<u64>,
) -> Result<Engine, Error> {
    if let Some(limits) = limits {
        // The engine opens with the core's default limits; stricter host limits are checked here.
        opc::Package::open(data.clone(), &limits)?;
    }
    let sidecar = sidecar_at(sidecar, version)?;
    Engine::open(data, sidecar.as_deref())
}

/// Native half of `quillwork.Document`: JSON in, JSON out. Use the Python wrapper.
#[pyclass(module = "quillwork._native")]
pub struct JsonDocument {
    engine: Engine,
    /// The sidecar as of the last save, until the next apply.
    sidecar: Option<String>,
}

impl JsonDocument {
    fn save_both(&mut self, py: Python<'_>) -> PyResult<(Vec<u8>, String)> {
        let engine = &mut self.engine;
        let (bytes, sidecar) = py.detach(move || engine.save()).py()?;
        self.sidecar = Some(sidecar.clone());
        Ok((bytes, sidecar))
    }
}

#[pymethods]
impl JsonDocument {
    /// Open a .docx (GIL released). `sidecar` is what `save_with_sidecar` / `sidecar` returned;
    /// the version is `max(version, the sidecar's)`, and 1 for a fresh upload.
    #[staticmethod]
    #[pyo3(signature = (data, limits=None, *, sidecar=None, version=None))]
    fn open(
        py: Python<'_>,
        data: PyBackedBytes,
        limits: Option<PyRef<'_, Limits>>,
        sidecar: Option<&str>,
        version: Option<u64>,
    ) -> PyResult<JsonDocument> {
        let limits = limits.map(|l| Limits::core_or_default(Some(&*l)));
        let engine = py.detach(move || open_engine(data.to_vec(), limits, sidecar, version)).py()?;
        Ok(JsonDocument { engine, sidecar: None })
    }

    /// Apply a Transaction (ops.schema.json); returns an OpResult or error envelope as JSON.
    fn apply_json(&mut self, py: Python<'_>, transaction: &str) -> String {
        self.sidecar = None;
        let engine = &mut self.engine;
        py.detach(move || engine.apply(transaction))
    }

    /// Apply a Transaction; returns the spec's §9 text form of the result or the error.
    fn apply_text(&mut self, py: Python<'_>, transaction: &str) -> String {
        self.sidecar = None;
        let engine = &mut self.engine;
        py.detach(move || engine.apply_text(transaction))
    }

    /// Run a ReadOp (ops.schema.json); returns a ReadResult or error envelope as JSON.
    fn read_json(&self, py: Python<'_>, request: &str) -> String {
        let engine = &self.engine;
        py.detach(move || engine.read(request))
    }

    /// The view model (view-model.schema.json) or an error envelope, as JSON.
    fn view_model_json(&self, py: Python<'_>) -> String {
        let engine = &self.engine;
        py.detach(move || engine.view_model_json())
    }

    /// The current version; every committed transaction bumps it.
    #[getter]
    fn version(&self) -> u64 {
        self.engine.version()
    }

    /// The document as .docx bytes (GIL released). Also captures the sidecar.
    fn save<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let (bytes, _) = self.save_both(py)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// The sidecar JSON for the document as it is now (saves first unless nothing changed since
    /// the last save).
    fn sidecar(&mut self, py: Python<'_>) -> PyResult<String> {
        match &self.sidecar {
            Some(s) => Ok(s.clone()),
            None => Ok(self.save_both(py)?.1),
        }
    }

    /// `(docx bytes, sidecar JSON)` in one save (GIL released).
    fn save_with_sidecar<'py>(&mut self, py: Python<'py>) -> PyResult<(Bound<'py, PyBytes>, String)> {
        let (bytes, sidecar) = self.save_both(py)?;
        Ok((PyBytes::new(py, &bytes), sidecar))
    }

    /// The change-set log as a JSON array of view-model `ChangeSet` objects.
    fn change_sets_json(&self) -> String {
        Value::Array(self.engine.change_sets().iter().map(|c| c.to_json()).collect()).to_string()
    }

    /// Markup-mode projection lines of `handles` (GIL released).
    fn render(&self, py: Python<'_>, handles: Vec<String>) -> Vec<String> {
        let engine = &self.engine;
        py.detach(move || engine.render(&handles))
    }

    /// The default revision author (a transaction's `author` overrides it).
    fn set_agent(&mut self, author: &str) {
        self.engine.set_agent(author);
    }

    /// Fix the clock revisions are stamped with (Unix seconds); `None` = the system clock.
    #[pyo3(signature = (unix_secs=None))]
    fn set_now(&mut self, unix_secs: Option<i64>) {
        self.engine.set_now(unix_secs);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn docs() -> Vec<PathBuf> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
        let mut v: Vec<PathBuf> = ["fixtures", "synthetic"]
            .iter()
            .flat_map(|d| std::fs::read_dir(root.join(d)).into_iter().flatten())
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "docx" || x == "docm"))
            .collect();
        v.sort();
        assert!(!v.is_empty(), "no corpus documents");
        v
    }

    fn anchors_of(e: &Engine) -> Sidecar {
        e.anchors().sidecar(e.doc())
    }

    #[test]
    fn an_empty_sidecar_numbers_like_a_fresh_mint() {
        for p in docs() {
            let bytes = std::fs::read(&p).unwrap();
            let Ok(minted) = Engine::open(bytes.clone(), None) else { continue };
            let opened = open_engine(bytes, None, None, None).unwrap();
            assert_eq!(opened.version(), FIRST_VERSION, "{}", p.display());
            assert_eq!(anchors_of(&opened), anchors_of(&minted), "{}", p.display());
        }
    }

    #[test]
    fn open_reaches_the_requested_version() {
        let bytes = std::fs::read(&docs()[0]).unwrap();
        let mut e = open_engine(bytes.clone(), None, None, Some(4)).unwrap();
        assert_eq!(e.version(), 4);
        let (saved, sidecar) = e.save().unwrap();
        assert_eq!(open_engine(saved.clone(), None, Some(&sidecar), None).unwrap().version(), 4);
        assert_eq!(open_engine(saved.clone(), None, Some(&sidecar), Some(6)).unwrap().version(), 6);
        assert!(matches!(
            open_engine(saved, None, Some(&sidecar), Some(3)),
            Err(Error::Edit { code: "BAD_VERSION", .. })
        ));
        // A bare anchors sidecar keeps its anchors and gets the requested version.
        let bare = anchors_of(&e).to_json();
        let reopened = open_engine(bytes, None, Some(&bare), Some(2)).unwrap();
        assert_eq!((reopened.version(), anchors_of(&reopened)), (2, anchors_of(&e)));
    }
}
