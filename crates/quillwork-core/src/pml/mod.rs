//! Lossless editing of existing PowerPoint presentations.
//!
//! A presentation is a branded source package, not an object model to rebuild. This initial
//! API edits text in existing shapes, table cells and speaker notes. It preserves run
//! properties and every unrelated XML/ZIP entry. PowerPoint has no Word-style tracked
//! changes: hosts review the original/candidate hashes and [`Presentation::review`] report.
//! Hosts must retain original bytes for rejection and bind approval to the candidate hash.
//!
//! Anchors use native slide and shape ids (`s256.sh2.p1`), not slide positions or ZIP names.
//! They survive text-only saves and slide reordering. Paragraph/table indices are positional;
//! always guard reopened/external packages with `base_sha256` as well as `base_version`.

mod model;

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::opc::{self, Package};
use crate::xml::ns;
use crate::{Error, Result};

pub(super) const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
/// The initial presentation API contract version; independent of package release versions.
pub const SPEC_VERSION: &str = "0.1.0";
const MAX_OPS: usize = 200;
const MAX_TEXT_BYTES: usize = 1_000_000;

pub(super) fn fail(code: &'static str, message: impl Into<String>) -> Error {
    Error::Edit { code, message: message.into() }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A lossless `.pptx` candidate. See module documentation for review and anchor semantics.
pub struct Presentation {
    package: Package,
    model: model::Model,
    original_text: BTreeMap<String, String>,
    version: u64,
    source_sha256: String,
    sha256: String,
    candidate: Option<Vec<u8>>,
}

impl Presentation {
    /// Open a transitional, nonmacro `.pptx`. No-op saves return the exact input bytes.
    /// Signed, write-protected, encrypted and macro-enabled packages are refused.
    /// Unknown content is preserved, not executed; external targets are listed, never fetched.
    pub fn open(bytes: Vec<u8>) -> Result<Self> {
        Self::open_with_limits(bytes, &opc::Limits::default())
    }

    /// Open with host-supplied package limits. The same limits remain in force for every
    /// mutation and candidate save; they are not merely an upload-time preflight.
    pub fn open_with_limits(bytes: Vec<u8>, limits: &opc::Limits) -> Result<Self> {
        let hash = digest(&bytes);
        let package = Package::open(bytes, limits)?;
        let model = model::parse(&package)?;
        let original_text = model.paragraphs.iter().map(|(a, p)| (a.clone(), p.text(&model.parts))).collect();
        Ok(Self {
            package,
            model,
            original_text,
            version: 1,
            source_sha256: hash.clone(),
            sha256: hash,
            candidate: None,
        })
    }

    /// Session version; starts at 1 on every open. Use `sha256` across process boundaries.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// SHA256 of the exact current candidate bytes.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// A compact text projection. Shape text is untrusted document data, JSON-quoted so control
    /// characters cannot create fake projection headers. Hidden slides/shapes are labelled.
    pub fn read(&self) -> String {
        let mut out = format!("PPTX version={} sha256={}\n", self.version, self.sha256);
        for (i, slide) in self.model.slides.iter().enumerate() {
            out.push_str(&format!("[{} SLIDE {}{}]\n", slide.id, i + 1, if slide.hidden { " HIDDEN" } else { "" }));
            for (notes, shapes) in [(false, &slide.shapes), (true, &slide.notes)] {
                for shape in shapes {
                    for anchor in &shape.paragraphs {
                        let para = &self.model.paragraphs[anchor];
                        let visibility_warnings = para.visibility_warnings(&self.model.parts);
                        out.push_str(&format!(
                            "[{}{}{}{}{}] {}\n",
                            anchor,
                            if notes { " NOTES" } else { "" },
                            if shape.hidden { " HIDDEN" } else { "" },
                            if para.editable { "" } else { " READ_ONLY" },
                            if visibility_warnings.is_empty() {
                                String::new()
                            } else {
                                format!(" VISIBILITY_WARNING={}", visibility_warnings.join(","))
                            },
                            serde_json::to_string(&para.text(&self.model.parts)).expect("string serializes")
                        ));
                    }
                }
            }
        }
        out
    }

    /// Structural metadata for agent selection. Geometry is source-local EMUs and may inherit
    /// from layouts/masters; it is not a layout renderer. Existing relationships are preserved.
    pub fn describe(&self) -> Result<Value> {
        let slides:Vec<_>=self.model.slides.iter().map(|slide|json!({
            "id":slide.id,"slide_id":slide.slide_id,"part":slide.part,"hidden":slide.hidden,
            "layout":slide.layout,"shapes":slide.shapes.iter().map(|s|s.describe(&self.model.paragraphs,&self.model.parts)).collect::<Vec<_>>(),
            "notes":slide.notes.iter().map(|s|s.describe(&self.model.paragraphs,&self.model.parts)).collect::<Vec<_>>()
        })).collect();
        Ok(json!({"spec_version":SPEC_VERSION,"format":"pptx","version":self.version,"sha256":self.sha256,
            "source_sha256":self.source_sha256,"review":"candidate","size":self.model.size,"slides":slides,
            "external_relationships":self.model.external,
            "capabilities":{"operations":["replace_text"],"native_tracked_changes":false,
                "existing_notes":true,"existing_table_text":true,"slide_structure":false,
                "fields_and_breaks_editable":false,"new_text_format":"first_changed_run",
                "inherited_visibility_resolved":false}}))
    }

    /// Return the exact current candidate package, with clean entries copied raw.
    pub fn save(&self) -> Result<Vec<u8>> {
        Ok(self.candidate.as_deref().unwrap_or(self.package.original_bytes()).to_vec())
    }

    /// Original/candidate hashes and cumulative paragraph text changes. This report is not an
    /// approval record, visual proof, or native PowerPoint tracked changes.
    pub fn review(&self) -> Result<Value> {
        let changes: Vec<_> = self
            .model
            .paragraphs
            .iter()
            .filter_map(|(anchor, para)| {
                let after = para.text(&self.model.parts);
                let before = &self.original_text[anchor];
                (after != *before).then(|| json!({"anchor":anchor,"part":para.part,"before":before,"after":after}))
            })
            .collect();
        Ok(json!({"spec_version":SPEC_VERSION,"version":self.version,"source_sha256":self.source_sha256,
            "candidate_sha256":self.sha256,"changes":changes,"native_tracked_changes":false}))
    }

    /// Atomically apply `replace_text` operations. Each operation matches one literal occurrence
    /// unless `occurrence` selects a 1-based match. `expect` guards the complete paragraph text.
    /// Inserted text inherits the first changed run; unchanged prefixes/suffixes retain their
    /// exact run properties. Paragraphs containing fields/breaks are readable but refused here.
    pub fn apply_tx(&mut self, tx: &Value) -> Result<Value> {
        let tx = object(tx, "transaction")?;
        keys(tx, &["base_version", "base_sha256", "ops", "spec_version"], "transaction")?;
        if let Some(v) = tx.get("spec_version")
            && v.as_str() != Some(SPEC_VERSION)
        {
            return Err(fail("INVALID_OP", format!("presentation spec_version must be {SPEC_VERSION}")));
        }
        let base = tx
            .get("base_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| fail("INVALID_OP", "base_version is required and must be an unsigned integer"))?;
        if base != self.version {
            return Err(fail(
                "CONFLICT",
                format!("base_version {base} does not match current version {}", self.version),
            ));
        }
        if let Some(hash) = tx.get("base_sha256") {
            let hash = hash
                .as_str()
                .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                .ok_or_else(|| fail("INVALID_OP", "base_sha256 must be a lowercase SHA256 hex digest"))?;
            if hash != self.sha256 {
                return Err(fail("CONFLICT", "base_sha256 does not match current candidate bytes"));
            }
        }
        let ops = tx
            .get("ops")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
            .ok_or_else(|| fail("INVALID_OP", "ops must be a nonempty array"))?;
        if ops.len() > MAX_OPS {
            return Err(fail("LIMIT", format!("at most {MAX_OPS} operations per transaction")));
        }
        let version = self.version.checked_add(1).ok_or_else(|| fail("LIMIT", "version counter exhausted"))?;
        let mut parts = self.model.parts.clone();
        let mut changed = Vec::new();
        let mut text_bytes = 0usize;
        for op in ops {
            let op = object(op, "operation")?;
            keys(op, &["op", "anchor", "find", "replace", "expect", "occurrence"], "operation")?;
            if string(op, "op")? != "replace_text" {
                return Err(fail("UNSUPPORTED", "only replace_text is implemented for presentations"));
            }
            let anchor = string(op, "anchor")?;
            let find = string(op, "find")?;
            let replace = string(op, "replace")?;
            text_bytes = text_bytes.saturating_add(find.len()).saturating_add(replace.len());
            if text_bytes > MAX_TEXT_BYTES {
                return Err(fail("LIMIT", format!("transaction text exceeds {MAX_TEXT_BYTES} bytes")));
            }
            if find.is_empty() {
                return Err(fail("INVALID_OP", "find must not be empty"));
            }
            if replace.chars().any(|c| matches!(c, '\n' | '\r' | '\t')) {
                return Err(fail("UNSUPPORTED", "replace_text does not add paragraph breaks or tabs"));
            }
            let para = self
                .model
                .paragraphs
                .get(anchor)
                .ok_or_else(|| fail("NOT_FOUND", format!("no text anchor {anchor}")))?;
            if !para.editable {
                return Err(fail(
                    "UNSUPPORTED",
                    format!("{anchor}: field, break, empty or unsupported paragraph; edit plain existing runs only"),
                ));
            }
            let before = para.text(&parts);
            if let Some(expect) = op.get("expect") {
                let expect =
                    expect.as_str().ok_or_else(|| fail("INVALID_OP", "expect must be the full paragraph string"))?;
                if expect != before {
                    return Err(fail("STALE_ANCHOR", format!("{anchor}: paragraph text differs from expect")));
                }
            }
            let matches: Vec<_> = before.match_indices(find).map(|(i, _)| i).collect();
            let ordinal = op
                .get("occurrence")
                .map(|n| {
                    n.as_u64()
                        .filter(|n| *n >= 1)
                        .ok_or_else(|| fail("INVALID_OP", "occurrence must be an integer >=1"))
                })
                .transpose()?;
            if ordinal.is_none() && matches.len() > 1 {
                return Err(fail("AMBIGUOUS", format!("{anchor}: {} matches; select occurrence", matches.len())));
            }
            let ordinal = ordinal.unwrap_or(1);
            let start = usize::try_from(ordinal - 1)
                .ok()
                .and_then(|i| matches.get(i))
                .copied()
                .ok_or_else(|| fail("NOT_FOUND", format!("{anchor}: requested text occurrence not found")))?;
            let end = start + find.len();
            let doc = parts.get_mut(&para.part).expect("model paragraph part exists");
            let mut offset = 0usize;
            let mut first = true;
            for &node in &para.texts {
                let text = doc.text_content(node);
                let next = offset + text.len();
                if offset < end && next > start {
                    let lo = start.saturating_sub(offset);
                    let hi = end.min(next) - offset;
                    let mut updated = String::from(&text[..lo]);
                    if first {
                        updated.push_str(replace);
                        first = false;
                    }
                    updated.push_str(&text[hi..]);
                    if updated != text {
                        doc.set_text(node, &updated)?;
                        // OOXML text requires xml:space for edge whitespace. Existing values
                        // are retained; no unrelated attributes are normalized.
                        if updated.starts_with(char::is_whitespace) || updated.ends_with(char::is_whitespace) {
                            doc.set_attr_ns(node, ns::XML, "space", "preserve")?;
                        }
                    }
                }
                offset = next;
            }
            let after = para.text(&parts);
            if before != after {
                changed.push(json!({"anchor":anchor,"part":para.part,"before":before,"after":after}));
            }
        }
        // Stage package construction and enforce ZIP/XML size limits before committing any state.
        let mut package = Package::open(self.package.original_bytes().to_vec(), self.package.limits())?;
        for (name, doc) in &parts {
            if doc.is_modified() {
                package.set_part(name, doc.to_bytes())?;
            }
        }
        let bytes = package.save()?;
        let hash = digest(&bytes);
        self.model.parts = parts;
        self.version = version;
        self.sha256 = hash;
        self.candidate = Some(bytes);
        Ok(json!({"spec_version":SPEC_VERSION,"ok":true,"base_version":base,"version":version,
            "sha256":self.sha256,"source_sha256":self.source_sha256,"changed":changed}))
    }
}

fn object<'a>(v: &'a Value, name: &str) -> Result<&'a Map<String, Value>> {
    v.as_object().ok_or_else(|| fail("INVALID_OP", format!("{name} must be an object")))
}
fn keys(map: &Map<String, Value>, allowed: &[&str], name: &str) -> Result<()> {
    if let Some(key) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(fail("INVALID_OP", format!("unknown {name} field {key}")));
    }
    Ok(())
}
fn string<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    map.get(key).and_then(Value::as_str).ok_or_else(|| fail("INVALID_OP", format!("{key} must be a string")))
}
