//! What the renderers need to know about one drawing (`w:drawing`, `w:pict`, `w:object`): its
//! kind, size, wrap, alignment, alt text, crop and transform, and the media part behind it.

use crate::wml::PartIdx;
use crate::xml::{Document, NodeId, ns};

use super::ctx::DocCtx;

const DECORATIVE: &str = "http://schemas.microsoft.com/office/drawing/2017/decorative";
const EMU_PER_PT: f64 = 12_700.0;

/// One drawing, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageInfo {
    /// View-model `drawing`: `picture`, `chart`, `smartart`, `shape`, `ole`, `watermark`.
    pub drawing: &'static str,
    pub inline: bool,
    /// View-model `wrap`: `inline`, `square`, `tight`, `through`, `topAndBottom`, `none`, `behind`.
    pub wrap: &'static str,
    /// Size in EMU (after crop: the displayed extent).
    pub w: i64,
    pub h: i64,
    /// `center` / `right` (`left` and unknown are `None`).
    pub align: Option<&'static str>,
    pub alt: Option<String>,
    pub title: Option<String>,
    pub decorative: bool,
    /// `a:srcRect` l, t, r, b in 1/1000 percent.
    pub crop: Option<[i64; 4]>,
    /// 60 000ths of a degree.
    pub rotation: i64,
    pub flip_h: bool,
    pub flip_v: bool,
    /// The embedded media part (picture, or an OLE object's preview), resolved.
    pub media: Option<String>,
    /// The picture is linked (`r:link` / external target), never fetched.
    pub external: bool,
}

impl ImageInfo {
    /// Projection kind: `IMAGE`, `CHART`, `DIAGRAM`, `SHAPE`, `OLE`, `WATERMARK`.
    pub fn proj_kind(&self) -> &'static str {
        match self.drawing {
            "picture" => "IMAGE",
            "chart" => "CHART",
            "smartart" => "DIAGRAM",
            "ole" => "OLE",
            "watermark" => "WATERMARK",
            _ => "SHAPE",
        }
    }

    /// Projection wrap: `square|tight|through|top-bottom|behind|front`.
    pub fn proj_wrap(&self) -> Option<&'static str> {
        Some(match self.wrap {
            "inline" => return None,
            "topAndBottom" => "top-bottom",
            "none" => "front",
            w => w,
        })
    }
}

fn is(d: &Document, n: NodeId, nsu: &str, local: &str) -> bool {
    d.is(n, nsu, local)
}

fn find_desc(d: &Document, root: NodeId, nsu: &str, local: &str) -> Option<NodeId> {
    d.descendants(root).find(|&n| is(d, n, nsu, local))
}

fn int_attr(d: &Document, n: NodeId, name: &str) -> Option<i64> {
    d.attr(n, name).and_then(|v| v.trim().parse().ok())
}

/// Read drawing `n` (in `part`).
pub(crate) fn info(cx: &DocCtx, part: PartIdx, n: NodeId) -> ImageInfo {
    let d = cx.doc.part(part);
    let mut out = ImageInfo {
        drawing: "shape",
        inline: true,
        wrap: "inline",
        w: 0,
        h: 0,
        align: None,
        alt: None,
        title: None,
        decorative: false,
        crop: None,
        rotation: 0,
        flip_h: false,
        flip_v: false,
        media: None,
        external: false,
    };
    let local = d.local_name(n).unwrap_or("");
    match local {
        "drawing" => drawingml(cx, part, d, n, &mut out),
        _ => vml(cx, part, d, n, local == "object", &mut out),
    }
    if out.inline && out.align.is_none() {
        let p = d.ancestors(n).find(|&a| d.is(a, ns::W, "p"));
        if let Some(p) = p {
            out.align = match cx.para(part, p).justification.as_deref() {
                Some("center") => Some("center"),
                Some("right" | "end") => Some("right"),
                _ => None,
            };
        }
    }
    out.alt = out.alt.filter(|a| !a.trim().is_empty());
    out.title = out.title.filter(|a| !a.trim().is_empty());
    out
}

fn drawingml(cx: &DocCtx, part: PartIdx, d: &Document, n: NodeId, out: &mut ImageInfo) {
    let Some(frame) = d.child_elements(n).find(|&c| is(d, c, ns::WP, "inline") || is(d, c, ns::WP, "anchor")) else {
        return;
    };
    out.inline = d.local_name(frame) == Some("inline");
    if let Some(e) = d.find_child(frame, ns::WP, "extent") {
        out.w = int_attr(d, e, "cx").unwrap_or(0).max(0);
        out.h = int_attr(d, e, "cy").unwrap_or(0).max(0);
    }
    if let Some(pr) = d.find_child(frame, ns::WP, "docPr") {
        out.alt = d.attr(pr, "descr").map(|c| c.into_owned());
        out.title = d.attr(pr, "title").map(|c| c.into_owned());
        out.decorative = d.descendants(pr).any(|x| {
            d.namespace(x) == Some(DECORATIVE)
                && d.local_name(x) == Some("decorative")
                && matches!(d.attr(x, "val").as_deref(), Some("1" | "true"))
        });
        if d.attr(pr, "name").is_some_and(|nm| nm.contains("WaterMark") || nm.contains("Watermark")) {
            out.drawing = "watermark";
        }
    }
    if !out.inline {
        let behind = matches!(d.attr(frame, "behindDoc").as_deref(), Some("1" | "true"));
        out.wrap = "none";
        for c in d.child_elements(frame) {
            if d.namespace(c) != Some(ns::WP) {
                continue;
            }
            match d.local_name(c) {
                Some("wrapSquare") => out.wrap = "square",
                Some("wrapTight") => out.wrap = "tight",
                Some("wrapThrough") => out.wrap = "through",
                Some("wrapTopAndBottom") => out.wrap = "topAndBottom",
                Some("wrapNone") => out.wrap = if behind { "behind" } else { "none" },
                Some("positionH") => {
                    out.align =
                        d.find_child(c, ns::WP, "align").map(|a| d.text_content(a)).and_then(|a| match a.trim() {
                            "center" => Some("center"),
                            "right" | "outside" => Some("right"),
                            _ => None,
                        })
                }
                _ => {}
            }
        }
    }
    let gd = find_desc(d, frame, ns::A, "graphicData");
    let uri = gd.and_then(|g| d.attr(g, "uri").map(|c| c.into_owned())).unwrap_or_default();
    if out.drawing != "watermark" {
        out.drawing = if uri.ends_with("/picture") {
            "picture"
        } else if uri.ends_with("/chart") || uri.contains("chartex") {
            "chart"
        } else if uri.ends_with("/diagram") {
            "smartart"
        } else {
            "shape"
        };
    }
    if let Some(blip) = find_desc(d, frame, ns::A, "blip") {
        if let Some(rid) = d.attr_ns(blip, ns::R, "embed") {
            out.media = cx.doc.rel_target_part(part, &rid);
        }
        if out.media.is_none() && d.attr_ns(blip, ns::R, "link").is_some() {
            out.external = true;
        }
    }
    if let Some(sr) = find_desc(d, frame, ns::A, "srcRect") {
        let g = |k| int_attr(d, sr, k).unwrap_or(0);
        let c = [g("l"), g("t"), g("r"), g("b")];
        if c != [0; 4] {
            out.crop = Some(c);
        }
    }
    if let Some(x) = find_desc(d, frame, ns::A, "xfrm") {
        out.rotation = int_attr(d, x, "rot").unwrap_or(0);
        out.flip_h = matches!(d.attr(x, "flipH").as_deref(), Some("1" | "true"));
        out.flip_v = matches!(d.attr(x, "flipV").as_deref(), Some("1" | "true"));
    }
}

/// `width:100pt;height:2in` → EMU.
fn css_len(style: &str, key: &str) -> Option<i64> {
    let v = style.split(';').find_map(|kv| {
        let (k, v) = kv.split_once(':')?;
        (k.trim().eq_ignore_ascii_case(key)).then(|| v.trim().to_ascii_lowercase())
    })?;
    let (num, unit) = v.split_at(v.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(v.len()));
    let x: f64 = num.trim().parse().ok()?;
    let pt = match unit {
        "pt" | "" => x,
        "in" => x * 72.0,
        "cm" => x * 72.0 / 2.54,
        "mm" => x * 72.0 / 25.4,
        "px" => x * 0.75,
        "pc" => x * 12.0,
        "emu" => return Some(x as i64),
        _ => return None,
    };
    Some((pt * EMU_PER_PT).round() as i64)
}

fn vml(cx: &DocCtx, part: PartIdx, d: &Document, n: NodeId, object: bool, out: &mut ImageInfo) {
    let shape = d.descendants(n).find(|&x| {
        d.namespace(x) == Some(ns::V)
            && matches!(d.local_name(x), Some("shape" | "rect" | "roundrect" | "oval" | "line" | "group" | "image"))
    });
    out.drawing = if object { "ole" } else { "shape" };
    let Some(shape) = shape else { return };
    let style = d.attr(shape, "style").map(|c| c.into_owned()).unwrap_or_default();
    out.w = css_len(&style, "width").unwrap_or(0).max(0);
    out.h = css_len(&style, "height").unwrap_or(0).max(0);
    let pos = style
        .split(';')
        .find_map(|kv| kv.split_once(':').filter(|(k, _)| k.trim() == "position").map(|(_, v)| v.trim().to_owned()));
    if pos.as_deref() == Some("absolute") {
        out.inline = false;
        let behind = style.contains("z-index:-");
        out.wrap = if behind { "behind" } else { "none" };
        if let Some(wr) = d.descendants(shape).find(|&x| d.local_name(x) == Some("wrap")) {
            match d.attr(wr, "type").as_deref() {
                Some("square") => out.wrap = "square",
                Some("tight") => out.wrap = "tight",
                Some("through") => out.wrap = "through",
                Some("topAndBottom") => out.wrap = "topAndBottom",
                _ => {}
            }
        }
        out.align = style.split(';').find_map(|kv| {
            let (k, v) = kv.split_once(':')?;
            (k.trim() == "mso-position-horizontal").then(|| match v.trim() {
                "center" => Some("center"),
                "right" | "outside" => Some("right"),
                _ => None,
            })?
        });
    }
    let id = d.attr(shape, "id").map(|c| c.into_owned()).unwrap_or_default();
    out.alt = d.attr(shape, "alt").map(|c| c.into_owned());
    if let Some(img) = d.descendants(shape).find(|&x| d.is(x, ns::V, "imagedata")) {
        if !object {
            out.drawing = "picture";
        }
        if let Some(rid) = d.attr_ns(img, ns::R, "id").or_else(|| d.attr_ns(img, ns::R, "pict")) {
            out.media = cx.doc.rel_target_part(part, &rid);
            if out.media.is_none() && cx.doc.rel(part, &rid).is_some_and(|r| r.external) {
                out.external = true;
            }
        }
        if out.title.is_none() {
            out.title = d.attr_ns(img, ns::O, "title").map(|c| c.into_owned());
        }
    }
    if id.contains("WaterMark") || id.contains("Watermark") {
        out.drawing = "watermark";
    }
    if let Some(rot) = style
        .split(';')
        .find_map(|kv| kv.split_once(':').filter(|(k, _)| k.trim() == "rotation").map(|(_, v)| v.trim().to_owned()))
    {
        let deg: f64 = rot.trim_end_matches("fd").parse().unwrap_or(0.0);
        out.rotation = if rot.ends_with("fd") { (deg / 65_536.0 * 60_000.0) as i64 } else { (deg * 60_000.0) as i64 };
    }
}

/// Size in cm to one decimal: `4.0×1.5cm`.
pub(crate) fn cm(w: i64, h: i64) -> String {
    let f = |e: i64| (e as f64) / 360_000.0;
    format!("{:.1}×{:.1}cm", f(w), f(h))
}
