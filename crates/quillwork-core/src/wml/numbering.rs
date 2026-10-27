//! List numbering labels (DESIGN §4.4): which paragraphs are numbered and the label Word shows.
//!
//! Supported: `w:num` → `w:abstractNum` (with `w:numStyleLink` / `w:styleLink`), `w:lvlOverride`
//! (`w:startOverride` and full `w:lvl` replacements), `w:start`, `w:numFmt` (decimal family,
//! letters, roman, bullet, none; anything else falls back to decimal), `w:lvlText` with `%1…%9`,
//! `w:lvlRestart`, `w:isLgl`. Numbering reaches a paragraph directly (`w:pPr/w:numPr`) or through
//! its paragraph style chain (`w:pStyle` → `w:basedOn` …, default paragraph style when there is no
//! `w:pStyle`); `w:numId w:val="0"` removes it.
//!
//! Counters are per list: a `w:num` shares the counters of its abstract definition unless it has a
//! `w:startOverride`, in which case it gets its own sequence that starts at the override.
//! Labels are computed per story, in document order.

use std::collections::{HashMap, HashSet};

use super::doc::WordDoc;
use super::story::Story;
use super::tree::{child_on, child_val, is_w, parse_int, w_attr};
use super::view::{NumPr, Paragraph, paragraphs, read_num_pr};
use crate::xml::{Document, NodeId, ns};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Level {
    pub start: i64,
    pub num_fmt: String,
    pub lvl_text: Option<String>,
    /// `w:lvlRestart/@w:val` (`Some(0)` = never restart).
    pub restart: Option<i64>,
    pub is_lgl: bool,
    /// `w:pStyle` linking this level to a paragraph style.
    pub p_style: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct AbstractNum {
    levels: [Option<Level>; 9],
    num_style_link: Option<String>,
    style_link: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Num {
    abstract_id: i64,
    start_overrides: HashMap<usize, i64>,
    level_overrides: HashMap<usize, Level>,
}

#[derive(Debug, Clone, Default)]
struct StyleInfo {
    based_on: Option<String>,
    num_pr: Option<NumPr>,
}

/// The parsed numbering definitions plus the minimal style lookup they need.
#[derive(Debug, Clone, Default)]
pub struct Numbering {
    abstracts: HashMap<i64, AbstractNum>,
    nums: HashMap<i64, Num>,
    styles: HashMap<String, StyleInfo>,
    default_para_style: Option<String>,
}

/// The numbering a paragraph ends up with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveNum {
    pub num_id: i64,
    pub ilvl: usize,
}

/// A computed list label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListLabel {
    pub num_id: i64,
    pub ilvl: usize,
    /// The label as displayed (`"1.2.3"`, `"a)"`, `"•"`).
    pub text: String,
    /// `w:numFmt` of the level (`decimal`, `bullet`, …).
    pub num_fmt: String,
}

fn parse_level(d: &Document, lvl: NodeId) -> Level {
    Level {
        start: child_val(d, lvl, "start").and_then(|v| parse_int(&v)).unwrap_or(0),
        num_fmt: child_val(d, lvl, "numFmt").unwrap_or_else(|| "decimal".to_owned()),
        lvl_text: child_val(d, lvl, "lvlText"),
        restart: child_val(d, lvl, "lvlRestart").and_then(|v| parse_int(&v)),
        is_lgl: child_on(d, lvl, "isLgl"),
        p_style: child_val(d, lvl, "pStyle"),
    }
}

fn ilvl_of(d: &Document, lvl: NodeId) -> Option<usize> {
    w_attr(d, lvl, "ilvl").and_then(|v| v.trim().parse::<usize>().ok()).filter(|&i| i < 9)
}

impl Numbering {
    /// Parse `numbering.xml` and the bits of `styles.xml` numbering needs. Missing parts give an
    /// empty definition set (every paragraph unnumbered).
    pub fn load(doc: &WordDoc) -> Numbering {
        let mut out = Numbering::default();
        if let Some(p) = doc.numbering_part() {
            let d = doc.part(p);
            for n in d.child_elements(d.root()) {
                if is_w(d, n, "abstractNum") {
                    let Some(id) = w_attr(d, n, "abstractNumId").and_then(|v| parse_int(&v)) else { continue };
                    let mut a = AbstractNum {
                        num_style_link: child_val(d, n, "numStyleLink"),
                        style_link: child_val(d, n, "styleLink"),
                        ..Default::default()
                    };
                    for lvl in d.child_elements(n).filter(|&c| is_w(d, c, "lvl")) {
                        if let Some(i) = ilvl_of(d, lvl) {
                            a.levels[i] = Some(parse_level(d, lvl));
                        }
                    }
                    out.abstracts.entry(id).or_insert(a);
                } else if is_w(d, n, "num") {
                    let Some(id) = w_attr(d, n, "numId").and_then(|v| parse_int(&v)) else { continue };
                    let Some(aid) = child_val(d, n, "abstractNumId").and_then(|v| parse_int(&v)) else { continue };
                    let mut num = Num { abstract_id: aid, ..Default::default() };
                    for ov in d.child_elements(n).filter(|&c| is_w(d, c, "lvlOverride")) {
                        let Some(i) = ilvl_of(d, ov) else { continue };
                        if let Some(s) = child_val(d, ov, "startOverride").and_then(|v| parse_int(&v)) {
                            num.start_overrides.insert(i, s);
                        }
                        if let Some(lvl) = d.find_child(ov, ns::W, "lvl") {
                            num.level_overrides.insert(i, parse_level(d, lvl));
                        }
                    }
                    out.nums.entry(id).or_insert(num);
                }
            }
        }
        if let Some(p) = doc.styles_part() {
            let d = doc.part(p);
            for s in d.child_elements(d.root()).filter(|&c| is_w(d, c, "style")) {
                if w_attr(d, s, "type").as_deref().is_some_and(|t| t != "paragraph") {
                    continue;
                }
                let Some(id) = w_attr(d, s, "styleId") else { continue };
                if out.default_para_style.is_none()
                    && w_attr(d, s, "default").as_deref().is_some_and(|v| v == "1" || v == "true" || v == "on")
                {
                    out.default_para_style = Some(id.clone());
                }
                let info = StyleInfo {
                    based_on: child_val(d, s, "basedOn"),
                    num_pr: d.find_child(s, ns::W, "pPr").and_then(|ppr| read_num_pr(d, ppr)),
                };
                out.styles.entry(id).or_insert(info);
            }
        }
        out
    }

    /// The `numId`/`ilvl` a paragraph ends up with, `None` when unnumbered (incl. `numId` 0).
    pub fn effective(&self, d: &Document, p: Paragraph) -> Option<EffectiveNum> {
        let direct = p.num_pr(d);
        let style = p.style_id(d).or_else(|| self.default_para_style.clone());
        let mut num_id = direct.and_then(|n| n.num_id);
        let mut ilvl = direct.and_then(|n| n.ilvl);
        let mut chain: Vec<String> = Vec::new();
        if num_id.is_none() {
            // Walk the style chain for the first numPr with a numId; an ilvl met on the way
            // (closest first) applies if the paragraph has none.
            let mut seen = HashSet::new();
            let mut cur = style.clone();
            while let Some(sid) = cur {
                if !seen.insert(sid.clone()) {
                    break;
                }
                let Some(info) = self.styles.get(&sid) else { break };
                chain.push(sid.clone());
                if let Some(np) = info.num_pr {
                    if ilvl.is_none() {
                        ilvl = np.ilvl;
                    }
                    if np.num_id.is_some() {
                        num_id = np.num_id;
                        break;
                    }
                }
                cur = info.based_on.clone();
            }
        }
        let num_id = num_id?;
        if num_id == 0 {
            return None;
        }
        let ilvl = match ilvl {
            Some(i) => i.clamp(0, 8) as usize,
            None => {
                // Numbering from a style: the level linked (w:pStyle) to the closest style in
                // the paragraph's chain, else 0.
                let (abs, _) = self.resolve(num_id)?;
                chain
                    .iter()
                    .find_map(|s| {
                        abs.levels.iter().position(|l| l.as_ref().and_then(|l| l.p_style.as_deref()) == Some(s))
                    })
                    .unwrap_or(0)
            }
        };
        Some(EffectiveNum { num_id, ilvl })
    }

    /// The abstract definition for a num (following `numStyleLink`) and its id.
    fn resolve(&self, num_id: i64) -> Option<(&AbstractNum, i64)> {
        let num = self.nums.get(&num_id)?;
        let mut aid = num.abstract_id;
        let mut abs = self.abstracts.get(&aid)?;
        let mut hops = 0;
        while let Some(link) = abs.num_style_link.as_ref() {
            hops += 1;
            if hops > 8 {
                break;
            }
            let linked_num = self.styles.get(link).and_then(|s| s.num_pr).and_then(|n| n.num_id);
            let target = linked_num.and_then(|n| self.nums.get(&n)).map(|n| n.abstract_id).or_else(|| {
                self.abstracts.iter().find(|(_, a)| a.style_link.as_deref() == Some(link)).map(|(id, _)| *id)
            });
            match target.and_then(|t| self.abstracts.get(&t).map(|a| (t, a))) {
                Some((t, a)) if t != aid => {
                    aid = t;
                    abs = a;
                }
                _ => break,
            }
        }
        Some((abs, aid))
    }

    fn level(&self, num_id: i64, ilvl: usize) -> Option<Level> {
        let num = self.nums.get(&num_id)?;
        if let Some(l) = num.level_overrides.get(&ilvl) {
            return Some(l.clone());
        }
        self.resolve(num_id)?.0.levels[ilvl].clone()
    }

    /// Labels of a story's paragraphs, in document order: `(paragraph, label)`, `None` for
    /// unnumbered ones or ones whose numbering definition is missing.
    pub fn labels(&self, doc: &WordDoc, story: &Story) -> Vec<(Paragraph, Option<ListLabel>)> {
        let d = doc.part(story.part);
        let mut state: HashMap<ListKey, [Option<i64>; 9]> = HashMap::new();
        paragraphs(d, story.root)
            .into_iter()
            .map(|p| {
                let label = self.effective(d, p).and_then(|e| self.next_label(&mut state, e));
                (p, label)
            })
            .collect()
    }

    fn next_label(&self, state: &mut HashMap<ListKey, [Option<i64>; 9]>, e: EffectiveNum) -> Option<ListLabel> {
        let num = self.nums.get(&e.num_id)?;
        let (_, aid) = self.resolve(e.num_id)?;
        let key = if num.start_overrides.is_empty() { ListKey::Abstract(aid) } else { ListKey::Num(e.num_id) };
        let levels: Vec<Option<Level>> = (0..9).map(|i| self.level(e.num_id, i)).collect();
        let cur = levels[e.ilvl].clone()?;
        let start_of = |i: usize| -> i64 {
            num.start_overrides.get(&i).copied().unwrap_or_else(|| levels[i].as_ref().map_or(1, |l| l.start))
        };
        let counters = state.entry(key).or_insert([None; 9]);
        counters[e.ilvl] = Some(match counters[e.ilvl] {
            Some(v) => v + 1,
            None => start_of(e.ilvl),
        });
        // Deeper levels restart unless their lvlRestart says otherwise.
        for d in e.ilvl + 1..9 {
            let restart = levels[d].as_ref().and_then(|l| l.restart);
            let restarts = match restart {
                None => true,
                Some(0) => false,
                Some(r) => (e.ilvl as i64) < r,
            };
            if restarts {
                counters[d] = None;
            }
        }
        let values: Vec<i64> = (0..9).map(|i| counters[i].unwrap_or_else(|| start_of(i))).collect();
        let text = match cur.num_fmt.as_str() {
            "bullet" => cur.lvl_text.clone().unwrap_or_default(),
            _ => {
                let template = cur.lvl_text.clone().unwrap_or_else(|| format!("%{}", e.ilvl + 1));
                render_template(&template, |i| {
                    let fmt = if cur.is_lgl {
                        "decimal"
                    } else {
                        levels[i].as_ref().map_or("decimal", |l| l.num_fmt.as_str())
                    };
                    format_number(values[i], fmt)
                })
            }
        };
        Some(ListLabel { num_id: e.num_id, ilvl: e.ilvl, text, num_fmt: cur.num_fmt })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ListKey {
    Abstract(i64),
    Num(i64),
}

/// Replace `%1`…`%9` in an `lvlText` template.
fn render_template(t: &str, mut value: impl FnMut(usize) -> String) -> String {
    let mut out = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%'
            && let Some(d) = chars.peek().and_then(|d| d.to_digit(10)).filter(|d| (1..=9).contains(d))
        {
            chars.next();
            out.push_str(&value(d as usize - 1));
            continue;
        }
        out.push(c);
    }
    out
}

/// Format a counter value in a `w:numFmt`.
pub fn format_number(v: i64, fmt: &str) -> String {
    match fmt {
        "none" => String::new(),
        "bullet" => String::new(),
        "decimalZero" => {
            if (0..10).contains(&v) {
                format!("0{v}")
            } else {
                v.to_string()
            }
        }
        "lowerLetter" => letters(v, b'a'),
        "upperLetter" => letters(v, b'A'),
        "lowerRoman" => roman(v).to_lowercase(),
        "upperRoman" => roman(v),
        "ordinal" => format!("{v}{}", ordinal_suffix(v)),
        // decimal, decimalEnclosed*, cardinalText, ordinalText, chicago, … fall back to decimal.
        _ => v.to_string(),
    }
}

/// Word letter numbering: a…z, aa…zz, aaa… (the letter repeats).
fn letters(v: i64, base: u8) -> String {
    if v <= 0 {
        return v.to_string();
    }
    let idx = ((v - 1) % 26) as u8;
    let reps = ((v - 1) / 26 + 1) as usize;
    std::iter::repeat_n((base + idx) as char, reps.min(64)).collect()
}

fn roman(v: i64) -> String {
    if v <= 0 || v >= 4000 {
        return v.to_string();
    }
    const T: &[(i64, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut n = v;
    let mut s = String::new();
    for &(val, sym) in T {
        while n >= val {
            s.push_str(sym);
            n -= val;
        }
    }
    s
}

fn ordinal_suffix(v: i64) -> &'static str {
    match (v % 100, v % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    }
}
