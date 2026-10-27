//! The read side over the corpora: projection and view-model goldens for the oracle fixtures,
//! view-model and patch JSON against the schemas, every printed anchor resolving, the view
//! model's accepted text against the Word model's AcceptAll text, patch round trips, find, and
//! size/speed numbers.
//!
//! The private corpus holds real client documents: only numbers are printed, files are
//! labelled `private#<i>`, and assertions never print document text.
//!
//! `QUILLWORK_BLESS=1 cargo test -p quillwork-core --test project_corpus` rewrites the goldens.

mod xml_support;

use std::path::{Path, PathBuf};
use std::time::Instant;

use quillwork_core::anchors::Anchors;
use quillwork_core::project::vm::{Block, ViewModel};
use quillwork_core::project::{self, FindRequest, ProjectOptions, ReadRequest, View};
use quillwork_core::wml::view::paragraph_texts;
use quillwork_core::wml::{TextMode, WordDoc};
use serde_json::Value;

fn fixtures() -> Vec<(String, PathBuf, Value)> {
    let dir = xml_support::workspace_root().join("corpus/fixtures");
    let mut v: Vec<_> = std::fs::read_dir(&dir)
        .expect("fixtures dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "docx" || x == "docm"))
        .filter_map(|p| {
            let exp = p.with_extension("expect.json");
            let exp: Value = serde_json::from_slice(&std::fs::read(&exp).ok()?).ok()?;
            Some((p.file_name().unwrap().to_string_lossy().into_owned(), p, exp))
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// Every document the read side should handle: fixtures, synthetic, then private (labelled).
fn all_docs() -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = fixtures().into_iter().map(|(n, p, _)| (n, p)).collect();
    for p in xml_support::synthetic_docx() {
        v.push((p.file_name().unwrap().to_string_lossy().into_owned(), p));
    }
    for (i, p) in xml_support::private_docx().into_iter().enumerate() {
        v.push((format!("private#{i}"), p));
    }
    v
}

fn is_private(name: &str) -> bool {
    name.starts_with("private#")
}

struct Loaded {
    doc: WordDoc,
    anchors: Anchors,
    sheet: quillwork_core::wml::style::StyleSheet,
}

fn load(path: &Path) -> Loaded {
    let doc = WordDoc::open(std::fs::read(path).expect("read")).expect("open");
    let anchors = Anchors::mint(&doc);
    let sheet = project::style_sheet(&doc).expect("styles");
    Loaded { doc, anchors, sheet }
}

fn opts(name: &str) -> ProjectOptions {
    ProjectOptions { file_name: (!is_private(name)).then(|| name.to_owned()), ..ProjectOptions::default() }
}

fn read_all(l: &Loaded, name: &str, view: View) -> String {
    let req = ReadRequest { view, budget: Some(1_000_000), options: opts(name), ..ReadRequest::default() };
    project::read(&l.doc, &l.anchors, &l.sheet, &req).expect("read").text
}

// ------------------------------------------------------------------------------ schemas

fn schema_dir() -> PathBuf {
    xml_support::workspace_root().join("spec/schema")
}

fn validator(file: &str) -> jsonschema::Validator {
    let load = |f: &str| -> Value {
        serde_json::from_slice(&std::fs::read(schema_dir().join(f)).expect("schema")).expect("json")
    };
    let mut o = jsonschema::options();
    for f in ["common.schema.json", "view-model.schema.json", "patch.schema.json"] {
        let s = load(f);
        let id = s["$id"].as_str().expect("$id").to_owned();
        o = o.with_resource(id, jsonschema::Resource::from_contents(s).expect("resource"));
    }
    o.build(&load(file)).expect("schema compiles")
}

fn schema_errors(v: &jsonschema::Validator, json: &Value) -> Vec<String> {
    v.iter_errors(json).take(5).map(|e| format!("{} at {}", e, e.instance_path)).collect()
}

/// Schema errors without the offending values (safe to print for private documents).
fn schema_paths(v: &jsonschema::Validator, json: &Value) -> Vec<String> {
    v.iter_errors(json).take(5).map(|e| format!("{} (schema {})", e.instance_path, e.schema_path)).collect()
}

// ------------------------------------------------------------------------------ goldens

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/project_golden")
}

fn check_golden(file: &str, got: &str, failures: &mut Vec<String>) {
    let path = golden_dir().join(file);
    if std::env::var_os("QUILLWORK_BLESS").is_some() {
        std::fs::create_dir_all(golden_dir()).expect("golden dir");
        std::fs::write(&path, got).expect("write golden");
        return;
    }
    match std::fs::read_to_string(&path) {
        Ok(want) if want == got => {}
        Ok(want) => {
            let line = want
                .lines()
                .zip(got.lines())
                .position(|(a, b)| a != b)
                .unwrap_or(want.lines().count().min(got.lines().count()));
            failures.push(format!(
                "{file}: differs at line {}\n  want: {:?}\n  got:  {:?}",
                line + 1,
                want.lines().nth(line),
                got.lines().nth(line)
            ));
        }
        Err(_) => failures.push(format!("{file}: missing golden (run with QUILLWORK_BLESS=1)")),
    }
}

#[test]
fn fixture_goldens() {
    let mut failures = Vec::new();
    for (name, path, _) in fixtures() {
        let l = load(&path);
        let stem = name.rsplit_once('.').unwrap().0;
        check_golden(&format!("{stem}.accepted.txt"), &read_all(&l, &name, View::Accepted), &mut failures);
        check_golden(&format!("{stem}.markup.txt"), &read_all(&l, &name, View::Markup), &mut failures);
        let vm = project::view_model(&l.doc, &l.anchors, &l.sheet, &opts(&name)).expect("view model");
        let json = serde_json::to_string_pretty(&vm).unwrap() + "\n";
        check_golden(&format!("{stem}.vm.json"), &json, &mut failures);
    }
    assert!(failures.is_empty(), "{} golden mismatches:\n{}", failures.len(), failures.join("\n"));
}

// ------------------------------------------------------------------------------ view model

#[test]
fn view_models_match_schema() {
    let v = validator("view-model.schema.json");
    let mut n = 0;
    for (name, path) in all_docs() {
        let l = load(&path);
        let vm = project::view_model(&l.doc, &l.anchors, &l.sheet, &opts(&name)).expect("view model");
        let json = serde_json::to_value(&vm).unwrap();
        if is_private(&name) {
            let errs = schema_paths(&v, &json);
            assert!(errs.is_empty(), "{name}: view model violates schema: {errs:?}");
        } else {
            let errs = schema_errors(&v, &json);
            assert!(errs.is_empty(), "{name}: view model violates schema: {errs:#?}");
        }
        // It round-trips through serde.
        let back: ViewModel = serde_json::from_value(json).expect("deserialize");
        assert!(back == vm, "{name}: view model serde round trip");
        n += 1;
    }
    assert!(n >= 18);
}

#[test]
fn spec_examples_match_schema() {
    let ex = xml_support::workspace_root().join("spec/examples");
    for (file, schema) in [("view-model.json", "view-model.schema.json"), ("patch.json", "patch.schema.json")] {
        let json: Value = serde_json::from_slice(&std::fs::read(ex.join(file)).unwrap()).unwrap();
        let errs = schema_errors(&validator(schema), &json);
        assert!(errs.is_empty(), "{file}: {errs:#?}");
    }
    // The examples also deserialize into the Rust shapes.
    let vm: ViewModel =
        serde_json::from_slice(&std::fs::read(ex.join("view-model.json")).unwrap()).expect("example vm");
    let p: project::Patch =
        serde_json::from_slice(&std::fs::read(ex.join("patch.json")).unwrap()).expect("example patch");
    // The patch turns v41 into the example's v42: replaying it on the v42 tree relabelled as
    // v41 gives the v42 tree back.
    let mut base = vm.clone();
    base.version = p.base_version;
    p.apply(&mut base).expect("example patch applies to the example view model");
    assert!(base == vm, "example patch is consistent with the example view model");
}

/// Accepted text of a list of blocks in the text model, paragraphs concatenated.
fn vm_text(blocks: &[Block], out: &mut String) {
    for b in blocks {
        match b {
            Block::P(p) => {
                for i in &p.inlines {
                    out.push_str(i.accepted_text());
                }
            }
            Block::Table(t) => {
                for r in &t.rows {
                    for c in &r.cells {
                        vm_text(&c.blocks, out);
                    }
                }
            }
            Block::Image(_) => {}
        }
    }
}

/// Characters both sides agree to drop: the Word model counts note-reference marks, symbol
/// glyphs and the like that the view model carries as non-text inlines.
fn comparable(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace() && !matches!(c, '\u{fffc}' | '\u{200b}' | '\u{ad}' | '\u{2011}')).collect()
}

#[test]
fn accepted_text_matches_accept_all() {
    let mut checked = 0;
    for (name, path) in all_docs() {
        let l = load(&path);
        let vm = project::view_model(&l.doc, &l.anchors, &l.sheet, &opts(&name)).expect("view model");
        let body = &l.doc.stories()[0];
        let want = paragraph_texts(l.doc.part(body.part), body.root, TextMode::AcceptAll).concat();
        let mut got = String::new();
        vm_text(&vm.blocks, &mut got);
        let (w, g) = (comparable(&want), comparable(&got));
        if is_private(&name) {
            assert!(
                w == g,
                "{name}: accepted text differs from AcceptAll ({} vs {} chars)",
                w.chars().count(),
                g.chars().count()
            );
        } else {
            assert_eq!(g, w, "{name}: accepted text vs AcceptAll");
        }
        checked += 1;
    }
    assert!(checked >= 18);
}

// ------------------------------------------------------------------------------ anchors

/// The anchor a projection line starts with (`[b100 H1] …`, `  [t1.r2] …`, `[c1 …]`).
fn line_anchor(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('[')?;
    let end = rest.find([' ', ']'])?;
    let a = &rest[..end];
    let ok = a.starts_with(['b', 'h', 'f', 'e', 'c', 't', 'i'])
        && a.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    ok.then_some(a)
}

#[test]
fn printed_anchors_resolve() {
    let mut total = 0;
    for (name, path) in all_docs() {
        let l = load(&path);
        let mut n = 0;
        for view in [View::Accepted, View::Markup] {
            let text = read_all(&l, &name, view);
            for line in text.lines() {
                let in_outline = line.strip_prefix("§ outline: ");
                let anchors: Vec<&str> = match in_outline {
                    Some(o) => o.split(" · ").filter_map(line_anchor).collect(),
                    None => line_anchor(line).into_iter().collect(),
                };
                // A collapsed run of empty paragraphs prints `[first..last] (n empty)`.
                for a in anchors.iter().flat_map(|a| a.split("..")) {
                    assert!(l.anchors.resolve(&l.doc, a).is_some(), "{name}: printed anchor {a} does not resolve");
                    n += 1;
                }
            }
        }
        // Every view-model id that is an anchor resolves too.
        let vm = project::view_model(&l.doc, &l.anchors, &l.sheet, &opts(&name)).expect("view model");
        let mut ids = Vec::new();
        for b in &vm.blocks {
            b.anchors(&mut ids);
        }
        for s in vm.stories.values() {
            for b in &s.blocks {
                b.anchors(&mut ids);
            }
        }
        for c in &vm.comments {
            ids.push(c.id.clone());
            for b in &c.blocks {
                b.anchors(&mut ids);
            }
        }
        for a in &ids {
            assert!(l.anchors.resolve(&l.doc, a).is_some(), "{name}: view-model anchor {a} does not resolve");
        }
        n += ids.len();
        assert!(n > 0, "{name}: no anchors at all");
        total += n;
    }
    eprintln!("printed_anchors_resolve: {total} anchors checked");
}

/// Body block and table anchors of the lines of a projection (the first window also prints
/// the header/footer stories and the comment appendix), outline line skipped, first
/// occurrence only.
fn line_anchors(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    text.lines()
        .filter(|l| !l.starts_with("§ outline: "))
        .filter_map(line_anchor)
        .filter(|a| a.starts_with(['b', 't']))
        .filter(|a| seen.insert(a.to_string()))
        .map(str::to_owned)
        .collect()
}

/// Paging with a small budget and following `next` prints the same lines, in the same order,
/// as one unbounded read: the windowed range read renders only from the window start but
/// keeps the state (TOC groups, open comments, sections) the whole-story render has there.
#[test]
fn paging_matches_one_read() {
    let (mut windows, mut lines) = (0, 0);
    for (name, path) in all_docs() {
        let l = load(&path);
        for view in [View::Accepted, View::Markup] {
            let whole_text = read_all(&l, &name, view);
            let whole = line_anchors(&whole_text);
            let mut paged = String::new();
            let mut range: Option<String> = None;
            for i in 0.. {
                assert!(i < 100_000, "{name}: paging does not end");
                let req = ReadRequest {
                    view,
                    range: range.clone(),
                    budget: Some(project::MIN_BUDGET),
                    options: opts(&name),
                    ..ReadRequest::default()
                };
                let r = project::read(&l.doc, &l.anchors, &l.sheet, &req).expect("read");
                paged.push_str(&r.text);
                paged.push('\n');
                windows += 1;
                match r.next {
                    Some(n) => {
                        assert_ne!(range.as_deref(), Some(n.as_str()), "{name}: window {i} does not advance");
                        range = Some(n);
                    }
                    None => break,
                }
            }
            // Lines identical between the two, by anchor (counts only: never print the text).
            let by_anchor = |t: &str| -> std::collections::HashMap<String, String> {
                t.lines()
                    .filter_map(|x| {
                        line_anchor(x).filter(|a| a.starts_with(['b', 't'])).map(|a| (a.to_owned(), x.to_owned()))
                    })
                    .collect()
            };
            let (pa, wh) = (by_anchor(&paged), by_anchor(&whole_text));
            let differ: Vec<&String> = wh.iter().filter(|(a, x)| pa.get(*a) != Some(x)).map(|(a, _)| a).collect();
            assert!(
                differ.is_empty(),
                "{name} {}: lines differ between paged and whole reads at {differ:?}",
                view.as_str()
            );
            lines += wh.len();
            let paged = line_anchors(&paged);
            if paged != whole {
                let at = paged.iter().zip(&whole).position(|(a, b)| a != b).unwrap_or(paged.len().min(whole.len()));
                panic!(
                    "{name} {}: paged {} anchors, whole {}, first difference at {at}: {:?} vs {:?}",
                    view.as_str(),
                    paged.len(),
                    whole.len(),
                    paged.get(at),
                    whole.get(at)
                );
            }
        }
    }
    eprintln!("paging_matches_one_read: {windows} windows, {lines} anchored lines identical");
}

// ------------------------------------------------------------------------------ patches

/// Deterministic perturbations of a view model: drop, move and edit blocks in every story.
fn perturb(vm: &ViewModel, seed: usize) -> ViewModel {
    let mut out = vm.clone();
    out.version += 1;
    let mut stories: Vec<&mut Vec<Block>> = vec![&mut out.blocks];
    stories.extend(out.stories.values_mut().map(|s| &mut s.blocks));
    for (si, blocks) in stories.into_iter().enumerate() {
        let n = blocks.len();
        if n == 0 {
            continue;
        }
        let k = (seed + si) % n;
        match (seed + si) % 4 {
            0 => {
                blocks.remove(k);
            }
            1 => {
                let b = blocks.remove(k);
                blocks.insert((k * 7 + 3) % (blocks.len() + 1), b);
            }
            2 => blocks.reverse(),
            _ => {
                if let Block::P(p) = &mut blocks[k] {
                    p.inlines.truncate(p.inlines.len() / 2);
                }
                if n > 2 {
                    blocks.swap(0, n - 1);
                }
            }
        }
    }
    if !out.comments.is_empty() && seed.is_multiple_of(2) {
        out.comments.remove(0);
    }
    if seed.is_multiple_of(3) {
        out.meta.track_changes = !out.meta.track_changes;
    }
    out
}

#[test]
fn patches_round_trip_and_match_schema() {
    let v = validator("patch.schema.json");
    let mut vms = Vec::new();
    for (name, path) in all_docs() {
        let l = load(&path);
        vms.push((name.clone(), project::view_model(&l.doc, &l.anchors, &l.sheet, &opts(&name)).expect("view model")));
    }
    let mut n = 0;
    let check = |label: &str, before: &ViewModel, after: &ViewModel, n: &mut usize| {
        let p = project::patch(before, after, &[]);
        let json = serde_json::to_value(&p).unwrap();
        let errs = if is_private(label) { schema_paths(&v, &json) } else { schema_errors(&v, &json) };
        assert!(errs.is_empty(), "{label}: patch violates schema: {errs:#?}");
        let mut got = before.clone();
        p.apply(&mut got).unwrap_or_else(|e| {
            panic!(
                "{label}: patch does not apply: {}",
                if is_private(label) { "(private)".into() } else { e.to_string() }
            )
        });
        // Patches never remove assets (the viewer may keep stale ones): compare the rest.
        assert!(after.assets.iter().all(|(k, v)| got.assets.get(k) == Some(v)), "{label}: assets missing after patch");
        got.assets.retain(|k, _| after.assets.contains_key(k));
        assert!(got == *after, "{label}: patch(before, after) applied to before != after");
        *n += 1;
    };
    for (name, vm) in &vms {
        // No change → empty patch.
        let mut same = vm.clone();
        same.version += 1;
        assert!(project::patch(vm, &same, &[]).is_empty(), "{name}: identical view models give a non-empty patch");
        for seed in 0..6 {
            check(name, vm, &perturb(vm, seed), &mut n);
        }
    }
    // Whole-document replacement between unrelated fixtures (ids collide across documents).
    let public: Vec<&(String, ViewModel)> = vms.iter().filter(|(n, _)| !is_private(n)).collect();
    for w in public.windows(2) {
        let mut after = w[1].1.clone();
        after.version = w[0].1.version + 1;
        check(&format!("{}→{}", w[0].0, w[1].0), &w[0].1, &after, &mut n);
    }
    // changed_anchors forces a block into the patch even when it compares equal.
    let (_, vm) = &vms[0];
    let first = vm.blocks[0].id().to_owned();
    let mut same = vm.clone();
    same.version += 1;
    let p = project::patch(vm, &same, &[first.as_str()]);
    assert!(p.upserts.iter().any(|u| u.block.id() == first), "changed anchor upserted");
    eprintln!("patches_round_trip: {n} patches checked");
}

// ------------------------------------------------------------------------------ find

#[test]
fn find_over_fixtures() {
    for (name, path, _) in fixtures() {
        let l = load(&path);
        let vm = project::view_model(&l.doc, &l.anchors, &l.sheet, &opts(&name)).expect("view model");
        // Take the first word of at least 4 letters of the body and find it again.
        let mut text = String::new();
        vm_text(&vm.blocks, &mut text);
        let Some(word) = text.split(|c: char| !c.is_alphanumeric()).find(|w| w.chars().count() >= 4) else { continue };
        let req = FindRequest { query: word.to_lowercase(), ..FindRequest::default() };
        let r = project::find(&l.doc, &l.anchors, &l.sheet, &req).expect("find");
        assert!(r.total >= 1 && !r.hits.is_empty(), "{name}: {word:?} not found");
        for h in &r.hits {
            assert!(l.anchors.resolve(&l.doc, &h.anchor).is_some(), "{name}: hit anchor {} unresolved", h.anchor);
            assert!(h.text.to_lowercase() == word.to_lowercase(), "{name}: hit text {:?}", h.text);
            assert!(h.snippet.contains(&h.text), "{name}: snippet lacks match");
            assert_eq!(h.end - h.start, h.text.encode_utf16().count(), "{name}: UTF-16 span");
        }
        assert!(!r.lines.is_empty(), "{name}: no projection lines");
        let txt = r.text(&req.query);
        assert!(txt.starts_with("§ find \"") && txt.lines().count() == 1 + r.lines.len(), "{name}: find text");
        // The offsets index the paragraph's text model.
        let h = &r.hits[0];
        if h.story == "b" {
            let para = vm.blocks.iter().find_map(|b| match b {
                Block::P(p) if p.id == h.anchor => Some(p),
                _ => None,
            });
            if let Some(p) = para {
                let t: String = p.inlines.iter().map(|i| i.accepted_text()).collect();
                let u: Vec<u16> = t.encode_utf16().collect();
                assert_eq!(String::from_utf16(&u[h.start..h.end]).unwrap(), h.text, "{name}: offsets");
            }
        }
        // Case sensitivity and whole words.
        let upper = FindRequest { query: word.to_uppercase(), case_sensitive: true, ..FindRequest::default() };
        let ru = project::find(&l.doc, &l.anchors, &l.sheet, &upper).expect("find");
        if word.to_uppercase() != word {
            assert!(ru.total <= r.total, "{name}: case-sensitive finds more");
        }
        let ww = FindRequest { query: word.to_lowercase(), whole_word: true, ..FindRequest::default() };
        assert!(
            project::find(&l.doc, &l.anchors, &l.sheet, &ww).expect("find").total <= r.total,
            "{name}: whole_word finds more"
        );
    }
}

#[test]
fn find_markup_sees_deletions() {
    let path = xml_support::workspace_root().join("corpus/fixtures/08-tracked-changes.docx");
    let l = load(&path);
    let markup = read_all(&l, "08-tracked-changes.docx", View::Markup);
    // Pick deleted text from the markup projection: `{--text--}`.
    let Some(del) = markup.split("{--").nth(1).and_then(|s| s.split("--}").next()) else { return };
    let del = del.trim();
    if del.is_empty() {
        return;
    }
    let acc = FindRequest { query: del.to_owned(), case_sensitive: true, ..FindRequest::default() };
    let mk = FindRequest { view: View::Markup, ..acc.clone() };
    let ra = project::find(&l.doc, &l.anchors, &l.sheet, &acc).expect("find");
    let rm = project::find(&l.doc, &l.anchors, &l.sheet, &mk).expect("find");
    assert!(rm.hits.iter().any(|h| h.deleted), "markup find flags the deleted match");
    assert!(ra.hits.iter().all(|h| !h.deleted), "accepted find never flags deletions");
    assert!(rm.total > ra.total || ra.total == 0 || rm.total >= ra.total);
}

// ------------------------------------------------------------------------------ assets

#[test]
fn assets_are_content_addressed() {
    for (name, path) in all_docs() {
        let l = load(&path);
        let vm = project::view_model(&l.doc, &l.anchors, &l.sheet, &opts(&name)).expect("view model");
        let assets = project::assets(&l.doc);
        for a in &assets {
            assert!(a.id.len() == 18 && a.id.starts_with("a_"), "{name}: asset id shape");
            let (_, bytes) = project::asset_bytes(&l.doc, &a.id).expect("asset bytes");
            assert_eq!(project::asset_id(&bytes), a.id, "{name}: id is sha256 of the bytes");
            assert_eq!(bytes.len(), a.bytes_len);
            let emf = matches!(a.content_type.as_str(), "image/x-emf" | "image/emf" | "image/x-wmf" | "image/wmf");
            assert!(!emf || a.needs_conversion, "{name}: EMF/WMF flagged");
        }
        // Every asset the view model references exists.
        for id in vm.assets.keys() {
            assert!(assets.iter().any(|a| &a.id == id), "{name}: view-model asset {id} unknown");
        }
    }
}

// ------------------------------------------------------------------------------ numbers

/// Size and speed: tokens per page, anchor share, render time. Prints numbers only. The time
/// limit is enforced in release builds (`cargo test --release`).
#[test]
fn size_and_speed() {
    let mut biggest: Option<(String, usize, f64, f64)> = None;
    for (name, path) in all_docs() {
        let l = load(&path);
        let paras = l.anchors.handles(0).count();
        let t0 = Instant::now();
        let text = read_all(&l, &name, View::Accepted);
        let read_ms = t0.elapsed().as_secs_f64() * 1e3;
        let t1 = Instant::now();
        let vm = project::view_model(&l.doc, &l.anchors, &l.sheet, &opts(&name)).expect("view model");
        let vm_ms = t1.elapsed().as_secs_f64() * 1e3;
        let pages = vm.meta.pages_estimate.unwrap_or(1).max(1) as usize;
        let tokens = project::estimate_tokens(&text);
        let anchor_chars: usize = text
            .lines()
            .filter_map(|l| {
                let s = l.trim_start();
                s.starts_with('[').then(|| s.find("] ").map_or(0, |i| i + 2))
            })
            .sum();
        let share = anchor_chars as f64 * 100.0 / text.chars().count().max(1) as f64;
        eprintln!(
            "{name}: {paras} paragraphs, {pages} pages, ~{tokens} tokens ({} per page), anchors {share:.1}% of chars, read {read_ms:.1} ms, view model {vm_ms:.1} ms",
            tokens / pages
        );
        if biggest.as_ref().is_none_or(|b| paras > b.1) {
            biggest = Some((name, paras, read_ms, vm_ms));
        }
    }
    let (name, paras, read_ms, vm_ms) = biggest.expect("documents");
    eprintln!("largest: {name}, {paras} paragraphs, read {read_ms:.1} ms, view model {vm_ms:.1} ms");
    if !cfg!(debug_assertions) {
        assert!(read_ms < 50.0, "{name}: read took {read_ms:.1} ms");
        assert!(vm_ms < 50.0, "{name}: view model took {vm_ms:.1} ms");
    }
}

/// Feeds projections to an external token counter without touching disk:
/// `cargo test -p quillwork-core --test project_corpus emit_projections -- --ignored --nocapture
/// | python count.py`. Each document is `\x1e<label>\x1f<text>`; private ones are labelled by
/// index. Never redirect this into a file.
#[test]
#[ignore]
fn emit_projections() {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    for (name, path) in all_docs() {
        let l = load(&path);
        let vm = project::view_model(&l.doc, &l.anchors, &l.sheet, &opts(&name)).expect("view model");
        let pages = vm.meta.pages_estimate.unwrap_or(1);
        for view in [View::Accepted, View::Markup] {
            let text = read_all(&l, &name, view);
            write!(out, "\x1e{name}|{}|{pages}\x1f{text}", view.as_str()).unwrap();
        }
    }
    out.flush().unwrap();
}
