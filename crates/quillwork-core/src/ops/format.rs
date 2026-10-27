//! `set_text_format`: run formatting on a found span or a whole block, tracked as `w:rPrChange`.
//!
//! A mark set to `true` writes the toggle; `false` removes it, or writes `w:val="0"` when a style
//! could switch it on; `null` removes the direct formatting so the style shows through.

use serde::Deserialize;
use serde_json::{Map, Value};

use super::para::block_runs;
use super::text::{handle_for, in_locked_control, isolate, locked_error};
use super::{Cx, OpError, OpResult, Tk, args, find, resolve};
use crate::edit::{EditOutcome, Tracker};
use crate::track::write::{self as tw, RPR_ORDER, is_w, w_attr, w_child};
use crate::xml::{Document, NodeId};

/// Word's highlight colour names (`ST_HighlightColor`).
const HIGHLIGHTS: &[&str] = &[
    "black",
    "blue",
    "cyan",
    "green",
    "magenta",
    "red",
    "yellow",
    "white",
    "darkBlue",
    "darkCyan",
    "darkGreen",
    "darkMagenta",
    "darkRed",
    "darkYellow",
    "darkGray",
    "lightGray",
    "none",
];

/// `ST_Underline`.
const UNDERLINES: &[&str] = &[
    "single",
    "words",
    "double",
    "thick",
    "dotted",
    "dottedHeavy",
    "dash",
    "dashedHeavy",
    "dashLong",
    "dashLongHeavy",
    "dotDash",
    "dashDotHeavy",
    "dotDotDash",
    "dashDotDotHeavy",
    "wave",
    "wavyHeavy",
    "wavyDouble",
    "none",
];

/// One validated mark change.
#[derive(Debug, Clone, PartialEq)]
enum Mark {
    /// A toggle (`b`, `i`, `strike`, `caps`, `smallCaps`): on, off, or cleared.
    Toggle(&'static [&'static str], Option<bool>),
    Underline(Option<String>),
    VertAlign(&'static str, Option<bool>),
    Highlight(Option<String>),
    Color(Option<String>),
    /// Half-points.
    Size(Option<u32>),
    Font(Option<String>),
}

fn parse_marks(m: &Map<String, Value>, path: &dyn Fn(&str) -> String) -> OpResult<Vec<Mark>> {
    if m.is_empty() {
        return Err(OpError::invalid(path("marks"), "marks needs at least one mark"));
    }
    let bool_of = |k: &str, v: &Value| -> OpResult<Option<bool>> {
        match v {
            Value::Null => Ok(None),
            Value::Bool(b) => Ok(Some(*b)),
            _ => Err(OpError::invalid(path(&format!("marks/{k}")), format!("{k} is true, false or null"))),
        }
    };
    let str_of = |k: &str, v: &Value| -> OpResult<Option<String>> {
        match v {
            Value::Null => Ok(None),
            Value::String(s) if !s.trim().is_empty() => Ok(Some(s.trim().to_owned())),
            _ => Err(OpError::invalid(path(&format!("marks/{k}")), format!("{k} is a string or null"))),
        }
    };
    let mut out = Vec::new();
    for (k, v) in m {
        let mark = match k.as_str() {
            "bold" => Mark::Toggle(&["b", "bCs"], bool_of(k, v)?),
            "italic" => Mark::Toggle(&["i", "iCs"], bool_of(k, v)?),
            "strike" => Mark::Toggle(&["strike"], bool_of(k, v)?),
            "caps" => Mark::Toggle(&["caps"], bool_of(k, v)?),
            "small_caps" => Mark::Toggle(&["smallCaps"], bool_of(k, v)?),
            "superscript" => Mark::VertAlign("superscript", bool_of(k, v)?),
            "subscript" => Mark::VertAlign("subscript", bool_of(k, v)?),
            "underline" => match v {
                Value::Null => Mark::Underline(None),
                Value::Bool(true) => Mark::Underline(Some("single".to_owned())),
                Value::Bool(false) => Mark::Underline(Some("none".to_owned())),
                Value::String(s) => match UNDERLINES.iter().find(|u| u.eq_ignore_ascii_case(s)) {
                    Some(u) => Mark::Underline(Some((*u).to_owned())),
                    None => {
                        return Err(OpError::invalid(
                            path("marks/underline"),
                            format!(
                                "unknown underline style {s:?} (single, double, thick, dotted, dash, wave, words, none, …)"
                            ),
                        ));
                    }
                },
                _ => {
                    return Err(OpError::invalid(path("marks/underline"), "underline is true, false, a style or null"));
                }
            },
            "highlight" => match str_of(k, v)? {
                None => Mark::Highlight(None),
                Some(s) => match HIGHLIGHTS.iter().find(|h| h.eq_ignore_ascii_case(&s)) {
                    Some(h) => Mark::Highlight(Some((*h).to_owned())),
                    None => {
                        return Err(OpError::invalid(
                            path("marks/highlight"),
                            format!("unknown highlight {s:?}; Word highlights are {}", HIGHLIGHTS.join(", ")),
                        ));
                    }
                },
            },
            "color" => match str_of(k, v)? {
                None => Mark::Color(None),
                Some(s) => {
                    let hex = s.strip_prefix('#').unwrap_or(&s);
                    if hex.eq_ignore_ascii_case("auto") {
                        Mark::Color(Some("auto".to_owned()))
                    } else if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                        Mark::Color(Some(hex.to_ascii_uppercase()))
                    } else {
                        return Err(OpError::invalid(path("marks/color"), "color is \"#RRGGBB\" or null"));
                    }
                }
            },
            "size" => match v {
                Value::Null => Mark::Size(None),
                Value::Number(n) => match n.as_f64() {
                    Some(pt) if (1.0..=819.0).contains(&pt) => Mark::Size(Some((pt * 2.0).round() as u32)),
                    _ => return Err(OpError::invalid(path("marks/size"), "size is in points, 1 to 819")),
                },
                _ => return Err(OpError::invalid(path("marks/size"), "size is a number of points or null")),
            },
            "font" => Mark::Font(str_of(k, v)?),
            other => {
                return Err(OpError::invalid(
                    path(&format!("marks/{other}")),
                    format!(
                        "unknown mark {other:?}; marks are bold, italic, underline, strike, superscript, subscript, caps, small_caps, highlight, color, size, font"
                    ),
                ));
            }
        };
        out.push(mark);
    }
    Ok(out)
}

fn remove_el(d: &mut Document, parent: NodeId, local: &str) -> crate::Result<()> {
    if let Some(c) = w_child(d, parent, local) {
        d.remove(c)?;
    }
    Ok(())
}

fn set_val(d: &mut Document, rpr: NodeId, local: &str, val: &str) -> crate::Result<NodeId> {
    let el = tw::ensure_child(d, rpr, local, RPR_ORDER)?;
    tw::set_w(d, el, "val", val)?;
    Ok(el)
}

/// Apply `marks` to one `rPr`. `styled`: a character or paragraph style may switch toggles on.
fn apply(d: &mut Document, rpr: NodeId, marks: &[Mark], styled: bool) -> crate::Result<()> {
    for m in marks {
        match m {
            Mark::Toggle(locals, v) => {
                for local in locals.iter() {
                    match v {
                        Some(true) => {
                            let el = tw::ensure_child(d, rpr, local, RPR_ORDER)?;
                            tw::remove_w(d, el, "val")?;
                        }
                        Some(false) if styled => {
                            set_val(d, rpr, local, "0")?;
                        }
                        Some(false) | None => remove_el(d, rpr, local)?,
                    }
                }
            }
            Mark::Underline(v) => match v.as_deref() {
                None => remove_el(d, rpr, "u")?,
                Some("none") if !styled => remove_el(d, rpr, "u")?,
                Some(s) => {
                    let el = set_val(d, rpr, "u", s)?;
                    if s == "none" {
                        tw::remove_w(d, el, "color")?;
                    }
                }
            },
            Mark::VertAlign(kind, v) => {
                let current = w_child(d, rpr, "vertAlign").and_then(|e| w_attr(d, e, "val"));
                match v {
                    Some(true) => {
                        set_val(d, rpr, "vertAlign", kind)?;
                    }
                    Some(false) | None => {
                        if current.as_deref() == Some(*kind) {
                            remove_el(d, rpr, "vertAlign")?;
                        }
                    }
                }
            }
            Mark::Highlight(v) => match v {
                None => remove_el(d, rpr, "highlight")?,
                Some(h) => {
                    set_val(d, rpr, "highlight", h)?;
                }
            },
            Mark::Color(v) => match v {
                None => remove_el(d, rpr, "color")?,
                Some(c) => {
                    let el = set_val(d, rpr, "color", c)?;
                    for k in ["themeColor", "themeTint", "themeShade"] {
                        tw::remove_w(d, el, k)?;
                    }
                }
            },
            Mark::Size(v) => match v {
                None => {
                    remove_el(d, rpr, "sz")?;
                    remove_el(d, rpr, "szCs")?;
                }
                Some(hp) => {
                    set_val(d, rpr, "sz", &hp.to_string())?;
                    set_val(d, rpr, "szCs", &hp.to_string())?;
                }
            },
            Mark::Font(v) => match v {
                None => {
                    if let Some(f) = w_child(d, rpr, "rFonts") {
                        for k in ["ascii", "hAnsi", "cs", "asciiTheme", "hAnsiTheme", "cstheme"] {
                            tw::remove_w(d, f, k)?;
                        }
                        if d.attrs(f).next().is_none() {
                            d.remove(f)?;
                        }
                    }
                }
                Some(name) => {
                    let f = tw::ensure_child(d, rpr, "rFonts", RPR_ORDER)?;
                    for k in ["ascii", "hAnsi", "cs"] {
                        tw::set_w(d, f, k, name)?;
                    }
                    for k in ["asciiTheme", "hAnsiTheme", "cstheme"] {
                        tw::remove_w(d, f, k)?;
                    }
                }
            },
        }
    }
    Ok(())
}

/// Is `run` inside a `w:ins` this transaction wrote (no snapshot needed)?
fn in_own_insert(d: &Document, tk: &Tk, run: NodeId) -> bool {
    d.ancestors(run).take_while(|&a| !is_w(d, a, "p")).any(|a| is_w(d, a, "ins") && tk.is_own(d, a))
}

/// Change one `rPr` (snapshotting it first in track mode).
fn format_rpr(
    d: &mut Document,
    tk: &mut Tk,
    rpr: NodeId,
    own: bool,
    marks: &[Mark],
    styled: bool,
) -> crate::Result<()> {
    if tk.track() && !own && w_child(d, rpr, "rPrChange").is_none() {
        let id = tk.rev();
        tw::snapshot_rpr(d, rpr, id, &tk.stamp)?;
    }
    apply(d, rpr, marks, styled)
}

fn is_styled(d: &Document, p: NodeId, rpr: Option<NodeId>) -> bool {
    rpr.is_some_and(|r| w_child(d, r, "rStyle").is_some())
        || w_child(d, p, "pPr").is_some_and(|ppr| w_child(d, ppr, "pStyle").is_some())
}

#[derive(Deserialize)]
struct FormatArgs {
    anchor: String,
    marks: Map<String, Value>,
    #[serde(default)]
    find: Option<String>,
    #[serde(default)]
    occurrence: Option<u32>,
}

/// `set_text_format {anchor, find?, occurrence?, marks}`.
pub fn set_text_format(cx: &mut Cx, op: &Value) -> OpResult<EditOutcome> {
    let a: FormatArgs = args(cx.index, op)?;
    let index = cx.index;
    let marks = parse_marks(&a.marks, &|k| format!("/ops/{index}/{k}"))?;
    let r = resolve(cx.doc, cx.anchors, cx.index, "anchor", &a.anchor)?;
    let mut tracker = Tracker::new(cx.anchors);
    let rev0 = cx.tk.revisions.len();
    match &a.find {
        Some(f) => {
            let (p, start, end) = {
                let d = cx.doc.part(r.part);
                let texts = find::texts(d, &r.paras);
                let hits = find::find_all(&texts, f);
                let path = cx.path("occurrence");
                let h = find::pick(&hits, &texts, f, a.occurrence, &a.anchor, path, &|i| {
                    handle_for(cx, r.part, r.paras[i])
                })?;
                (r.paras[h.para], h.start, h.end)
            };
            if in_locked_control(cx.doc.part(r.part), p) {
                return Err(locked_error(&a.anchor));
            }
            let d = cx.doc.part_mut(r.part);
            let runs = isolate(d, cx.tk, p, start, end, &a.anchor)?;
            for run in runs {
                let own = in_own_insert(d, cx.tk, run);
                let rpr = tw::ensure_rpr(d, run)?;
                let styled = is_styled(d, p, Some(rpr));
                format_rpr(d, cx.tk, rpr, own, &marks, styled)?;
            }
            tracker.touch(cx.doc, r.part, p);
        }
        None => {
            {
                let d = cx.doc.part(r.part);
                if r.paras.iter().any(|&p| in_locked_control(d, p)) {
                    return Err(locked_error(&a.anchor));
                }
            }
            let d = cx.doc.part_mut(r.part);
            for &p in &r.paras {
                for run in block_runs(d, p) {
                    let has_text = d.child_elements(run).any(|c| !is_w(d, c, "rPr"));
                    if !has_text {
                        continue;
                    }
                    let own = in_own_insert(d, cx.tk, run);
                    let rpr = tw::ensure_rpr(d, run)?;
                    let styled = is_styled(d, p, Some(rpr));
                    format_rpr(d, cx.tk, rpr, own, &marks, styled)?;
                }
                // The paragraph mark takes the formatting too (as selecting the paragraph in Word).
                let ppr = tw::ensure_ppr(d, p)?;
                let mark = tw::ensure_child(d, ppr, "rPr", tw::PPR_ORDER)?;
                let own = tw::mark_revision(d, p, "ins").is_some_and(|m| cx.tk.is_own(d, m));
                let styled = is_styled(d, p, Some(mark));
                format_rpr(d, cx.tk, mark, own, &marks, styled)?;
            }
            for &p in &r.paras {
                tracker.touch(cx.doc, r.part, p);
            }
        }
    }
    Ok(tracker.finish(cx.doc, cx.anchors, cx.tk.revisions[rev0..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(k: &str) -> String {
        format!("/ops/0/{k}")
    }

    #[test]
    fn marks_validate() {
        let m: Map<String, Value> =
            serde_json::from_str(r##"{"bold":true,"color":"#ff0000","size":10.5,"underline":"double"}"##).unwrap();
        let v = parse_marks(&m, &p).unwrap();
        assert!(v.contains(&Mark::Color(Some("FF0000".into()))));
        assert!(v.contains(&Mark::Size(Some(21))));
        assert!(v.contains(&Mark::Underline(Some("double".into()))));
        let bad: Map<String, Value> = serde_json::from_str(r#"{"blink":true}"#).unwrap();
        assert_eq!(parse_marks(&bad, &p).unwrap_err().path.as_deref(), Some("/ops/0/marks/blink"));
        let bad: Map<String, Value> = serde_json::from_str(r#"{"highlight":"orange"}"#).unwrap();
        assert_eq!(parse_marks(&bad, &p).unwrap_err().code, "INVALID_OP");
    }
}
