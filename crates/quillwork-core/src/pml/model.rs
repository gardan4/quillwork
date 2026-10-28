use std::collections::{BTreeMap, HashSet};

use serde_json::{Value, json};

use crate::opc::{Package, Relationship};
use crate::xml::{Document, NodeId, ns};
use crate::{Error, Result};

use super::{P, fail};

pub(super) const PRESENTATION_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml";
const SLIDE_TYPE: &str = "application/vnd.openxmlformats-officedocument.presentationml.slide+xml";
const NOTES_TYPE: &str = "application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml";

#[derive(Clone)]
pub(super) struct Paragraph {
    pub anchor: String,
    pub part: String,
    pub node: NodeId,
    pub texts: Vec<NodeId>,
    pub editable: bool,
    pub visibility_warnings: Vec<&'static str>,
}

impl Paragraph {
    pub fn text(&self, parts: &BTreeMap<String, Document>) -> String {
        let doc = &parts[&self.part];
        let mut out = String::new();
        for n in doc.child_elements(self.node) {
            if doc.is(n, ns::A, "br") {
                out.push('\n');
            } else if doc.is(n, ns::A, "r") || doc.is(n, ns::A, "fld") {
                // Only a direct text child is rendered text. Arbitrary extension metadata
                // inside rPr may itself contain DrawingML-namespaced elements; including
                // those would disagree with the editor's direct a:t offset map.
                for t in doc.child_elements(n).filter(|&t| doc.is(t, ns::A, "t")) {
                    out.push_str(&doc.text_content(t));
                }
            }
        }
        out
    }

    pub fn describe(&self, parts: &BTreeMap<String, Document>) -> Value {
        json!({"anchor": self.anchor, "text": self.text(parts), "editable": self.editable,
            "visibility_warnings": self.visibility_warnings(parts)})
    }

    pub fn visibility_warnings(&self, parts: &BTreeMap<String, Document>) -> Vec<&'static str> {
        let mut warnings = self.visibility_warnings.clone();
        if self.text(parts).chars().any(|c| matches!(c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}')) {
            warnings.push("zero_width_text");
        }
        warnings
    }
}

pub(super) struct Shape {
    pub id: String,
    pub shape_id: u32,
    pub name: String,
    pub alt_text: Option<String>,
    pub kind: String,
    pub hidden: bool,
    pub placeholder: Option<String>,
    pub parent: Option<String>,
    pub paragraphs: Vec<String>,
    pub geometry: Value,
}

impl Shape {
    pub fn describe(&self, paras: &BTreeMap<String, Paragraph>, parts: &BTreeMap<String, Document>) -> Value {
        json!({
            "id": self.id, "shape_id": self.shape_id, "name": self.name,
            "kind": self.kind, "hidden": self.hidden, "placeholder": self.placeholder,
            "alt_text": self.alt_text,
            "parent": self.parent, "geometry": self.geometry,
            "paragraphs": self.paragraphs.iter().map(|a| paras[a].describe(parts)).collect::<Vec<_>>()
        })
    }
}

pub(super) struct Slide {
    pub id: String,
    pub slide_id: u32,
    pub part: String,
    pub hidden: bool,
    pub shapes: Vec<Shape>,
    pub notes: Vec<Shape>,
    pub layout: Option<String>,
}

pub(super) struct Model {
    pub parts: BTreeMap<String, Document>,
    pub paragraphs: BTreeMap<String, Paragraph>,
    pub slides: Vec<Slide>,
    pub external: Vec<Value>,
    pub size: Value,
}

fn load(pkg: &Package, name: &str, content_type: &str, root: &str) -> Result<Document> {
    if pkg.content_type_of(name) != Some(content_type) {
        return Err(Error::Package(format!("{name}: expected {content_type}")));
    }
    let doc = Document::parse(&pkg.part(name)?)?;
    if !doc.is(doc.root(), P, root) {
        return Err(Error::Package(format!("{name}: expected PresentationML {root} root")));
    }
    Ok(doc)
}

fn rels(pkg: &Package, part: &str) -> Result<Vec<Relationship>> {
    let rels = pkg.rels_of(part)?;
    let mut ids = HashSet::new();
    for rel in &rels {
        if rel.id.is_empty() || !ids.insert(&rel.id) {
            return Err(Error::Package(format!("{part}: missing or duplicate relationship id")));
        }
    }
    Ok(rels)
}

fn target(pkg: &Package, part: &str, rel: &Relationship) -> Result<String> {
    pkg.resolve(part, rel)
        .filter(|name| pkg.contains(name))
        .ok_or_else(|| Error::Package(format!("{part}: missing, external or invalid target {}", rel.id)))
}

pub(super) fn parse(pkg: &Package) -> Result<Model> {
    for name in pkg.names() {
        let lower = name.to_ascii_lowercase();
        let ct = pkg.content_type_of(name).unwrap_or_default().to_ascii_lowercase();
        if lower.starts_with("_xmlsignatures/") || ct.contains("digital-signature") {
            return Err(fail("PROTECTED", "signed presentations are not editable"));
        }
        if lower.ends_with("vbaproject.bin") || ct.contains("macroenabled") || ct.contains("vbaproject") {
            return Err(fail("UNSUPPORTED", "macro-enabled presentations are not supported"));
        }
    }
    let root_rels = rels(pkg, "")?;
    let office: Vec<_> = root_rels.iter().filter(|r| r.rel_type == format!("{}/officeDocument", ns::R)).collect();
    if office.len() != 1 {
        return Err(Error::Package("expected one internal presentation relationship".into()));
    }
    let main = target(pkg, "", office[0])?;
    let doc = load(pkg, &main, PRESENTATION_TYPE, "presentation")?;
    if doc.descendants_named(doc.root(), P, "modifyVerifier").next().is_some() {
        return Err(fail("PROTECTED", "write-protected presentations are not editable"));
    }
    let main_rels = rels(pkg, &main)?;
    let mut out = Model {
        parts: BTreeMap::new(),
        paragraphs: BTreeMap::new(),
        slides: Vec::new(),
        external: Vec::new(),
        size: doc
            .find_child(doc.root(), P, "sldSz")
            .map(|n| json!({"cx": doc.attr(n, "cx"), "cy": doc.attr(n, "cy")}))
            .unwrap_or(Value::Null),
    };
    // Keep every external target as data; no code in this module dereferences a URL.
    for name in pkg.names().filter(|name| name.ends_with(".rels")) {
        let source = crate::opc::source_of_rels(name).unwrap_or_default();
        for rel in rels(pkg, &source)? {
            if rel.external {
                out.external.push(json!({"part": source, "id": rel.id, "type": rel.rel_type, "target": rel.target}));
            }
        }
    }
    let Some(list) = doc.find_child(doc.root(), P, "sldIdLst") else { return Ok(out) };
    let mut ids = HashSet::new();
    let mut slide_parts = HashSet::new();
    let mut notes_parts = HashSet::new();
    for n in doc.child_elements(list) {
        if !doc.is(n, P, "sldId") {
            return Err(Error::Package("unexpected element in slide id list".into()));
        }
        let id = doc
            .attr(n, "id")
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|n| (256..=2_147_483_647).contains(n))
            .ok_or_else(|| Error::Package("invalid native slide id".into()))?;
        if !ids.insert(id) {
            return Err(Error::Package(format!("duplicate native slide id {id}")));
        }
        let rid = doc.attr_ns(n, ns::R, "id").ok_or_else(|| Error::Package("slide has no relationship id".into()))?;
        let rel = main_rels
            .iter()
            .find(|r| r.id == rid && r.rel_type == format!("{}/slide", ns::R))
            .ok_or_else(|| Error::Package(format!("slide {id}: missing slide relationship")))?;
        let name = target(pkg, &main, rel)?;
        if !slide_parts.insert(name.to_ascii_lowercase()) {
            return Err(Error::Package("multiple slide ids reference the same part".into()));
        }
        let slide_doc = load(pkg, &name, SLIDE_TYPE, "sld")?;
        let hidden = slide_doc.attr(slide_doc.root(), "show").is_some_and(|v| matches!(&*v, "0" | "false"));
        let prefix = format!("s{id}");
        let shapes = parse_shapes(&slide_doc, &name, &prefix, false, &mut out.paragraphs)?;
        out.parts.insert(name.clone(), slide_doc);
        let slide_rels = rels(pkg, &name)?;
        let layout = slide_rels
            .iter()
            .find(|r| r.rel_type == format!("{}/slideLayout", ns::R))
            .map(|r| target(pkg, &name, r))
            .transpose()?;
        let notes: Vec<_> = slide_rels.iter().filter(|r| r.rel_type == format!("{}/notesSlide", ns::R)).collect();
        if notes.len() > 1 {
            return Err(Error::Package("slide has multiple notes parts".into()));
        }
        let note_shapes = if let Some(rel) = notes.first() {
            let note_name = target(pkg, &name, rel)?;
            if !notes_parts.insert(note_name.to_ascii_lowercase()) {
                return Err(Error::Package("notes part is shared by multiple slides".into()));
            }
            let note_doc = load(pkg, &note_name, NOTES_TYPE, "notes")?;
            let shapes = parse_shapes(&note_doc, &note_name, &format!("{prefix}.n"), true, &mut out.paragraphs)?;
            out.parts.insert(note_name, note_doc);
            shapes
        } else {
            Vec::new()
        };
        out.slides.push(Slide { id: prefix, slide_id: id, part: name, hidden, shapes, notes: note_shapes, layout });
    }
    Ok(out)
}

fn parse_shapes(
    doc: &Document,
    part: &str,
    prefix: &str,
    notes: bool,
    paras: &mut BTreeMap<String, Paragraph>,
) -> Result<Vec<Shape>> {
    let tree = doc
        .find_child(doc.root(), P, "cSld")
        .and_then(|n| doc.find_child(n, P, "spTree"))
        .ok_or_else(|| Error::Package(format!("{part}: missing shape tree")))?;
    let mut ids = HashSet::new();
    for n in doc.descendants_named(tree, P, "cNvPr") {
        let id = doc
            .attr(n, "id")
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|i| *i > 0)
            .ok_or_else(|| Error::Package(format!("{part}: invalid shape id")))?;
        if !ids.insert(id) {
            return Err(Error::Package(format!("{part}: duplicate shape id {id}")));
        }
    }
    let mut out = Vec::new();
    for shape in doc.child_elements(tree) {
        visit_shape(doc, part, prefix, notes, shape, None, paras, &mut out)?;
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn visit_shape(
    doc: &Document,
    part: &str,
    prefix: &str,
    notes: bool,
    node: NodeId,
    parent: Option<String>,
    paras: &mut BTreeMap<String, Paragraph>,
    out: &mut Vec<Shape>,
) -> Result<()> {
    if doc.namespace(node) != Some(P)
        || !matches!(doc.local_name(node), Some("sp" | "grpSp" | "graphicFrame" | "pic" | "cxnSp"))
    {
        return Ok(());
    }
    let nv_name = match doc.local_name(node) {
        Some("sp") => "nvSpPr",
        Some("grpSp") => "nvGrpSpPr",
        Some("graphicFrame") => "nvGraphicFramePr",
        Some("pic") => "nvPicPr",
        Some("cxnSp") => "nvCxnSpPr",
        _ => return Ok(()),
    };
    let nonvisual = doc
        .find_child(node, P, nv_name)
        .ok_or_else(|| Error::Package(format!("{part}: shape has no nonvisual properties")))?;
    let nv = doc
        .find_child(nonvisual, P, "cNvPr")
        .ok_or_else(|| Error::Package(format!("{part}: shape has no nonvisual properties")))?;
    let id: u32 =
        doc.attr(nv, "id").and_then(|s| s.parse().ok()).ok_or_else(|| Error::Package("invalid shape id".into()))?;
    let anchor = format!("{prefix}.sh{id}");
    let placeholder = doc
        .find_child(nonvisual, P, "nvPr")
        .and_then(|n| doc.find_child(n, P, "ph"))
        .map(|n| doc.attr(n, "type").map(|s| s.into_owned()).unwrap_or_else(|| "obj".into()));
    if notes && placeholder.as_deref().is_some_and(|s| matches!(s, "dt" | "ftr" | "hdr" | "sldNum" | "sldImg")) {
        return Ok(());
    }
    let mut anchors = Vec::new();
    if let Some(body) = doc.find_child(node, P, "txBody") {
        add_paragraphs(doc, part, body, &anchor, paras, &mut anchors)?;
    }
    if doc.is(node, P, "graphicFrame") {
        // A graphic frame can also contain chart/SmartArt payloads and opaque extension
        // metadata. Only this direct, table-typed DrawingML path owns editable table text.
        let table = doc
            .find_child(node, ns::A, "graphic")
            .and_then(|n| doc.find_child(n, ns::A, "graphicData"))
            .filter(|&n| {
                doc.attr(n, "uri").as_deref() == Some("http://schemas.openxmlformats.org/drawingml/2006/table")
            })
            .and_then(|n| doc.find_child(n, ns::A, "tbl"));
        if let Some(table) = table {
            for (ri, row) in doc.child_elements(table).filter(|&n| doc.is(n, ns::A, "tr")).enumerate() {
                for (ci, cell) in doc.child_elements(row).filter(|&n| doc.is(n, ns::A, "tc")).enumerate() {
                    if let Some(body) = doc.find_child(cell, ns::A, "txBody") {
                        add_paragraphs(
                            doc,
                            part,
                            body,
                            &format!("{anchor}.r{}.c{}", ri + 1, ci + 1),
                            paras,
                            &mut anchors,
                        )?;
                    }
                }
            }
        }
    }
    let xfrm = doc
        .find_child(node, P, "spPr")
        .and_then(|n| doc.find_child(n, ns::A, "xfrm"))
        .or_else(|| doc.find_child(node, P, "xfrm"));
    let geometry = xfrm
        .map(|n| {
            let off = doc.find_child(n, ns::A, "off");
            let ext = doc.find_child(n, ns::A, "ext");
            json!({"x": off.and_then(|n| doc.attr(n,"x")), "y": off.and_then(|n| doc.attr(n,"y")),
            "cx": ext.and_then(|n| doc.attr(n,"cx")), "cy": ext.and_then(|n| doc.attr(n,"cy")), "inherited": false})
        })
        .unwrap_or_else(|| json!({"inherited":true}));
    out.push(Shape {
        id: anchor.clone(),
        shape_id: id,
        name: doc.attr(nv, "name").map(|s| s.into_owned()).unwrap_or_default(),
        alt_text: doc.attr(nv, "descr").map(|s| s.into_owned()),
        kind: doc.local_name(node).unwrap_or_default().to_owned(),
        hidden: std::iter::once(node).chain(doc.ancestors(node)).any(|shape| {
            doc.child_elements(shape)
                .filter(|&n| doc.namespace(n) == Some(P) && doc.local_name(n).is_some_and(|s| s.starts_with("nv")))
                .filter_map(|n| doc.find_child(n, P, "cNvPr"))
                .any(|n| doc.attr(n, "hidden").is_some_and(|s| matches!(&*s, "1" | "true")))
        }),
        placeholder,
        parent,
        paragraphs: anchors,
        geometry,
    });
    if doc.is(node, P, "grpSp") {
        for child in doc.child_elements(node) {
            visit_shape(doc, part, prefix, notes, child, Some(anchor.clone()), paras, out)?;
        }
    }
    Ok(())
}

fn add_paragraphs(
    doc: &Document,
    part: &str,
    body: NodeId,
    prefix: &str,
    paras: &mut BTreeMap<String, Paragraph>,
    out: &mut Vec<String>,
) -> Result<()> {
    for (pi, node) in doc.child_elements(body).filter(|&n| doc.is(n, ns::A, "p")).enumerate() {
        let anchor = format!("{prefix}.p{}", pi + 1);
        let mut texts = Vec::new();
        let mut editable = true;
        for child in doc.child_elements(node) {
            if doc.is(child, ns::A, "r") {
                let ts: Vec<_> = doc.child_elements(child).filter(|&n| doc.is(n, ns::A, "t")).collect();
                if ts.len() != 1 {
                    editable = false;
                }
                if doc.child_elements(child).any(|n| !doc.is(n, ns::A, "rPr") && !doc.is(n, ns::A, "t")) {
                    editable = false;
                }
                texts.extend(ts);
            } else if !doc.is(child, ns::A, "pPr") && !doc.is(child, ns::A, "endParaRPr") {
                editable = false;
            }
        }
        if texts.is_empty() {
            editable = false;
        }
        let mut warnings = Vec::new();
        if doc
            .descendants_named(node, ns::A, "rPr")
            .any(|n| doc.attr(n, "sz").and_then(|s| s.parse::<u32>().ok()).is_some_and(|size| size <= 200))
        {
            warnings.push("tiny_text");
        }
        if doc
            .descendants_named(node, ns::A, "srgbClr")
            .any(|n| doc.attr(n, "val").is_some_and(|s| s.eq_ignore_ascii_case("FFFFFF")))
        {
            warnings.push("white_text");
        }
        if doc.descendants_named(node, ns::A, "alpha").any(|n| doc.attr(n, "val").is_some_and(|s| &*s == "0")) {
            warnings.push("transparent_text");
        }
        let para = Paragraph {
            anchor: anchor.clone(),
            part: part.to_owned(),
            node,
            texts,
            editable,
            visibility_warnings: warnings,
        };
        if paras.insert(anchor.clone(), para).is_some() {
            return Err(Error::Package(format!("duplicate text anchor {anchor}")));
        }
        out.push(anchor);
    }
    Ok(())
}
