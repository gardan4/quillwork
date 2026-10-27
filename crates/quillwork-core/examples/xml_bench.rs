//! Timing for the xml module on the largest `word/document.xml` among the given .docx files.
//!
//!     cargo run --release -p quillwork-core --example xml_bench -- a.docx b.docx ...
//!
//! Prints only sizes and timings (safe to run on private documents). For peak RSS, run the
//! built binary under `/usr/bin/time -l` (macOS) or `/usr/bin/time -v` (Linux).

use std::io::Read;
use std::time::{Duration, Instant};

use quillwork_core::xml::{Document, ns};

fn document_xml(path: &str) -> Option<Vec<u8>> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let mut e = z.by_name("word/document.xml").ok()?;
    let mut v = Vec::new();
    e.read_to_end(&mut v).ok()?;
    Some(v)
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn time<T>(iters: usize, mut f: impl FnMut() -> T) -> Duration {
    let mut v = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        std::hint::black_box(f());
        v.push(t.elapsed());
    }
    median(v)
}

fn main() {
    let src = std::env::args()
        .skip(1)
        .filter_map(|p| document_xml(&p))
        .max_by_key(|b| b.len())
        .expect("pass one or more .docx paths");
    let iters: usize = std::env::var("ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(50);
    let doc = Document::parse(&src).unwrap();
    println!("document.xml: {} bytes, {} nodes", src.len(), doc.node_count());

    let text = std::str::from_utf8(&src).unwrap();
    let tokenise = time(iters, || {
        // Baseline: quick-xml events alone, no tree, no validation beyond its own.
        let mut r = quick_xml::Reader::from_str(text);
        let mut n = 0usize;
        loop {
            match r.read_event() {
                Ok(quick_xml::events::Event::Eof) | Err(_) => break n,
                Ok(_) => n += 1,
            }
        }
    });
    let parse = time(iters, || Document::parse(&src).unwrap());
    let ser_fast = time(iters, || doc.to_bytes());
    let ser_walk = time(iters, || doc.to_bytes_walked());
    let edit = |d: &mut Document| {
        let t = d
            .descendants_named(d.root(), ns::W, "t")
            .nth(1000)
            .or_else(|| d.descendants_named(d.root(), ns::W, "t").last())
            .unwrap();
        d.set_text(t, "edited").unwrap();
    };
    let ser_edited = {
        let mut d = Document::parse(&src).unwrap();
        edit(&mut d);
        time(iters, || d.to_bytes())
    };
    let full_noop = time(iters, || Document::parse(&src).unwrap().to_bytes());
    let full_edit = time(iters, || {
        let mut d = Document::parse(&src).unwrap();
        edit(&mut d);
        d.to_bytes()
    });
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    println!("quick-xml tokenise (baseline) {:8.3} ms", ms(tokenise));
    println!("parse                         {:8.3} ms", ms(parse));
    println!("serialise, no-op (fast path)  {:8.3} ms", ms(ser_fast));
    println!("serialise, no-op (walked)     {:8.3} ms", ms(ser_walk));
    println!("serialise, one edit           {:8.3} ms", ms(ser_edited));
    println!("parse + serialise (no-op)     {:8.3} ms", ms(full_noop));
    println!("parse + 1 edit + serialise    {:8.3} ms", ms(full_edit));
}
