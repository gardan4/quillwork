//! quillwork-core: lossless, AI-friendly editing of Office Open XML documents.
//!
//! The Office file is the document; everything else is a view of it. See `docs/DESIGN.md`.
//!
//! Layering (lower modules never depend on higher ones):
//! `opc` (zip package) → `xml` (raw-span token tree) → `wml` (Word model views) →
//! `anchors` → `project` (LLM text + view model) → `track` / `ops` → `validate`.

pub mod anchors;
pub mod edit;
pub mod engine;
pub mod error;
pub mod opc;
pub mod ops;
pub mod pml;
pub mod project;
pub mod track;
pub mod validate;
pub mod wml;
pub mod xml;

pub use error::{Error, Result};
