//! Resolve every paragraph and run of every corpus document (`wml::style`).
//!
//! The private corpus holds real client documents: this test prints counts only. Font
//! families outside a list of common typefaces are reported as `other-N`, style ids never.
//!
//! `cargo test -p quillwork-core --release --test style_corpus -- --nocapture`

mod xml_support;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use quillwork_core::wml::style::{EffectiveParagraph, EffectiveRun, StyleSheet};
use quillwork_core::xml::{Document, NodeId, ns::W};

const KNOWN_FONTS: &[&str] = &[
    "Calibri",
    "Calibri Light",
    "Arial",
    "Arial Narrow",
    "Times New Roman",
    "Cambria",
    "Georgia",
    "Verdana",
    "Tahoma",
    "Segoe UI",
    "Helvetica",
    "Garamond",
    "Aptos",
    "Aptos Display",
    "Courier New",
    "Consolas",
    "Symbol",
    "Wingdings",
    "Century Gothic",
    "Trebuchet MS",
    "Open Sans",
    "Roboto",
    "Lato",
    "Montserrat",
    "Source Sans Pro",
    "Book Antiqua",
    "MS Mincho",
    "MS Gothic",
    "Yu Mincho",
    "Yu Gothic",
];

#[derive(Default)]
struct Stats {
    files: usize,
    paragraphs: usize,
    runs: usize,
    in_tables: usize,
    numbered: usize,
    list_labels: usize,
    with_font: usize,
    with_size: usize,
    bold: usize,
    italic: usize,
    coloured: usize,
    fonts: BTreeMap<String, usize>,
    sizes: BTreeMap<String, usize>,
    other_fonts: Vec<String>,
}

impl Stats {
    fn font_bucket(&mut self, f: Option<&str>) -> String {
        let Some(f) = f else { return "(none)".into() };
        if let Some(k) = KNOWN_FONTS.iter().find(|k| k.eq_ignore_ascii_case(f)) {
            return (*k).to_owned();
        }
        let i = match self.other_fonts.iter().position(|o| o == f) {
            Some(i) => i,
            None => {
                self.other_fonts.push(f.to_owned());
                self.other_fonts.len() - 1
            }
        };
        format!("other-{}", i + 1)
    }

    fn add_run(&mut self, r: &EffectiveRun) {
        self.runs += 1;
        self.with_font += usize::from(r.font.is_some());
        self.with_size += usize::from(r.size.is_some());
        self.bold += usize::from(r.bold == Some(true));
        self.italic += usize::from(r.italic == Some(true));
        self.coloured += usize::from(r.color.as_deref().is_some_and(|c| c != "auto" && c != "000000"));
        let fb = self.font_bucket(r.font.as_deref());
        *self.fonts.entry(fb).or_default() += 1;
        let sb = match r.size {
            None => "(none)".to_owned(),
            Some(hp) => format!("{:>5.1}pt", f64::from(hp) / 2.0),
        };
        *self.sizes.entry(sb).or_default() += 1;
    }

    fn print(&self, label: &str) {
        println!(
            "[{label}] files={} paragraphs={} runs={} in_tables={} numbered={} list_labels={} \
             runs_with_font={} runs_with_size={} bold={} italic={} coloured={}",
            self.files,
            self.paragraphs,
            self.runs,
            self.in_tables,
            self.numbered,
            self.list_labels,
            self.with_font,
            self.with_size,
            self.bold,
            self.italic,
            self.coloured
        );
        for (f, n) in &self.fonts {
            println!("[{label}]   font {f:<16} {n}");
        }
        for (s, n) in &self.sizes {
            println!("[{label}]   size {s:<16} {n}");
        }
    }
}

struct Package {
    sheet: StyleSheet,
    stories: Vec<Document>,
}

fn is_story(name: &str) -> bool {
    name == "word/document.xml"
        || ["word/header", "word/footer"].iter().any(|p| name.starts_with(p) && name.ends_with(".xml"))
        || name == "word/footnotes.xml"
        || name == "word/endnotes.xml"
}

fn load(path: &Path) -> Option<Package> {
    let parts = xml_support::xml_parts(path);
    let get = |n: &str| parts.iter().find(|(p, _)| p == n).map(|(_, b)| b.as_slice());
    let styles = Document::parse(get("word/styles.xml")?).expect("styles.xml parses");
    let theme = get("word/theme/theme1.xml").map(|b| Document::parse(b).expect("theme parses"));
    let mut sheet = StyleSheet::parse(&styles, theme.as_ref()).expect("stylesheet");
    if let Some(b) = get("word/numbering.xml") {
        sheet.set_numbering(&Document::parse(b).expect("numbering parses")).expect("numbering");
    }
    if let Some(b) = get("word/settings.xml") {
        sheet.set_settings(&Document::parse(b).expect("settings parses")).expect("settings");
    }
    let stories =
        parts.iter().filter(|(n, _)| is_story(n)).map(|(_, b)| Document::parse(b).expect("story parses")).collect();
    Some(Package { sheet, stories })
}

fn resolve_story(sheet: &StyleSheet, d: &Document, stats: Option<&mut Stats>) -> usize {
    let mut runs = 0;
    let mut stats = stats;
    let paras: Vec<NodeId> = d.descendants_named(d.root(), W, "p").collect();
    for p in paras {
        let ep: EffectiveParagraph = sheet.resolve_paragraph_at(d, p);
        if let Some(st) = stats.as_deref_mut() {
            st.paragraphs += 1;
            st.in_tables += usize::from(d.ancestors(p).any(|a| d.is(a, W, "tc")));
            st.numbered += usize::from(ep.numbering.is_some());
            st.list_labels += usize::from(sheet.resolve_list_label(d, p, &ep).is_some());
            if let Some(id) = ep.style.as_deref() {
                assert!(sheet.style_by_id(id).is_some(), "resolved paragraph style must exist");
            }
            let _ = sheet.resolve_paragraph_mark(d, p, &ep);
        }
        // Runs directly in this paragraph (also inside hyperlinks, smartTags, ins/del, …), not
        // in nested paragraphs (text boxes are their own paragraphs).
        for r in d.descendants_named(p, W, "r") {
            if d.ancestors(r).find(|&a| d.is(a, W, "p")) != Some(p) {
                continue;
            }
            let er = sheet.resolve_run(d, r, &ep);
            runs += 1;
            if let Some(st) = stats.as_deref_mut() {
                let json = serde_json::to_string(&er).expect("serialise run");
                let back: EffectiveRun = serde_json::from_str(&json).expect("deserialise run");
                assert!(back == er, "run JSON round trip");
                st.add_run(&er);
            }
        }
    }
    runs
}

fn run_corpus(label: &str, files: &[std::path::PathBuf]) -> Option<(usize, std::path::PathBuf)> {
    let mut stats = Stats::default();
    let mut biggest: Option<(usize, std::path::PathBuf)> = None;
    for f in files {
        let Some(pkg) = load(f) else { continue };
        stats.files += 1;
        let before = stats.runs;
        for d in &pkg.stories {
            resolve_story(&pkg.sheet, d, Some(&mut stats));
        }
        let n = stats.runs - before;
        if biggest.as_ref().is_none_or(|(b, _)| n > *b) {
            biggest = Some((n, f.clone()));
        }
    }
    stats.print(label);
    assert!(stats.files == 0 || stats.runs > 0, "[{label}] resolved no runs");
    biggest
}

fn time_biggest(label: &str, path: &Path) {
    let pkg = load(path).expect("biggest file loads");
    let mut best = Duration::MAX;
    let mut cold = Duration::ZERO;
    let mut runs = 0;
    for i in 0..5 {
        pkg.sheet.invalidate();
        let t = Instant::now();
        runs = pkg.stories.iter().map(|d| resolve_story(&pkg.sheet, d, None)).sum::<usize>();
        let e = t.elapsed();
        if i == 0 {
            cold = e;
        }
        best = best.min(e);
    }
    let t = Instant::now();
    let warm_runs: usize = pkg.stories.iter().map(|d| resolve_story(&pkg.sheet, d, None)).sum();
    let warm = t.elapsed();
    let (pc, rc) = pkg.sheet.cache_len();
    println!(
        "[{label}] biggest file: runs={runs} first={:.2}ms best_cold_cache={:.2}ms warm_cache={:.2}ms \
         ({warm_runs} runs) cache_entries=para:{pc} run:{rc}",
        cold.as_secs_f64() * 1e3,
        best.as_secs_f64() * 1e3,
        warm.as_secs_f64() * 1e3,
    );
}

#[test]
fn resolve_synthetic_corpus() {
    let files = xml_support::synthetic_docx();
    if files.is_empty() {
        eprintln!("[synthetic] corpus/synthetic has no .docx; skipping");
        return;
    }
    if let Some((_, p)) = run_corpus("synthetic", &files) {
        time_biggest("synthetic", &p);
    }
}

#[test]
fn resolve_private_corpus() {
    let files = xml_support::private_docx();
    if files.is_empty() {
        eprintln!("[private] private corpus absent; skipping");
        return;
    }
    if let Some((_, p)) = run_corpus("private", &files) {
        time_biggest("private", &p);
    }
}
