//! Time `Package::open` + saves on one file; prints numbers only (never content).
//!
//! `cargo run --release -p quillwork-core --example opc_bench -- <file.docx> [--levels]`
//! Run under `/usr/bin/time -l` for peak memory. `--levels` also reports how many deflated
//! parts zlib reproduces byte-for-byte at some level (what a re-deflate of an edit can match).

use std::io::{Read, Write};
use std::time::Instant;

use quillwork_core::opc::{Limits, Package};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: opc_bench <file.docx> [--levels]");
    let levels = args.any(|a| a == "--levels");
    let bytes = std::fs::read(&path).expect("read");
    let size = bytes.len();
    const N: usize = 50;

    let t = Instant::now();
    let mut pkg = Package::open(bytes.clone(), &Limits::default()).expect("open");
    let t_open = ms(t);
    let t = Instant::now();
    let out = pkg.save().expect("save");
    let t_noop = ms(t);
    assert!(out == bytes);
    drop(out);

    // Averages over N runs (the first open above includes cold caches).
    let t = Instant::now();
    for _ in 0..N {
        let p = Package::open(bytes.clone(), &Limits::default()).unwrap();
        std::hint::black_box(p.save().unwrap());
    }
    let t_avg = ms(t) / N as f64;

    let t = Instant::now();
    let names: Vec<String> = pkg.names().map(str::to_owned).collect();
    let mut total = 0usize;
    for n in &names {
        total += pkg.part(n).unwrap().len();
    }
    let t_read = ms(t);

    let t = Instant::now();
    let mut doc = pkg.part("word/document.xml").unwrap().into_owned();
    doc.extend_from_slice(b"<!-- quillwork -->");
    pkg.set_part("word/document.xml", doc).unwrap();
    let t_set = ms(t);
    let t = Instant::now();
    let edited = pkg.save().unwrap();
    let t_edit = ms(t);

    println!(
        "{size} bytes, {} entries, {total} bytes uncompressed\n\
         open {t_open:.3} ms, no-op save {t_noop:.3} ms (first run)\n\
         open + no-op save {t_avg:.3} ms (mean of {N})\n\
         read all parts {t_read:.3} ms\n\
         set_part(document.xml) incl. level guess {t_set:.3} ms, edited save {t_edit:.3} ms, edited size {}",
        names.len(),
        edited.len()
    );

    if levels {
        let mut za = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        let (mut deflated, mut matched) = (0, 0);
        let mut by_level = [0usize; 10];
        for i in 0..za.len() {
            let plain = {
                let mut f = za.by_index(i).unwrap();
                if f.compression() != zip::CompressionMethod::Deflated {
                    continue;
                }
                let mut v = Vec::new();
                f.read_to_end(&mut v).unwrap();
                v
            };
            let mut raw = Vec::new();
            za.by_index_raw(i).unwrap().read_to_end(&mut raw).unwrap();
            deflated += 1;
            for l in [6u32, 9, 1, 2, 3, 4, 5, 7, 8, 0] {
                let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(l));
                e.write_all(&plain).unwrap();
                if e.finish().unwrap() == raw {
                    matched += 1;
                    by_level[l as usize] += 1;
                    break;
                }
            }
        }
        println!("deflated parts reproduced exactly by zlib: {matched}/{deflated} (by level {by_level:?})");
    }
}
