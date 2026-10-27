//! Seeded randomized robustness test: mutated / truncated zips fed to `Package::open` (and,
//! when it succeeds, to every read / edit / save path) must never panic. Iterations scale with
//! `QUILLWORK_FUZZ_ITERS` (default 3000 per seed file).

mod opc_common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use opc_common::*;
use quillwork_core::opc::{Limits, Package};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

/// Byte offsets worth hitting: signatures and the header fields right after them.
fn hot_spots(b: &[u8]) -> Vec<usize> {
    let mut v = Vec::new();
    for p in 0..b.len().saturating_sub(4) {
        if b[p] == 0x50 && b[p + 1] == 0x4b && b[p + 2] < 9 && b[p + 3] < 9 {
            v.push(p);
        }
    }
    v
}

fn mutate(rng: &mut Rng, seed: &[u8], hot: &[usize]) -> Vec<u8> {
    let mut b = seed.to_vec();
    let n = 1 + rng.below(4);
    for _ in 0..n {
        if b.is_empty() {
            break;
        }
        // Prefer header fields: a random header plus 0..46 bytes.
        let at = if !hot.is_empty() && rng.below(3) > 0 {
            (hot[rng.below(hot.len())] + rng.below(46)).min(b.len() - 1)
        } else {
            rng.below(b.len())
        };
        match rng.below(7) {
            0 => b[at] ^= 1 << rng.below(8),
            1 => b[at] = rng.next() as u8,
            2 => b[at] = [0x00, 0xFF, 0x7F, 0x80][rng.below(4)],
            3 => {
                let v = [0u32, 0xFFFF_FFFF, 0x7FFF_FFFF, 1, b.len() as u32][rng.below(5)];
                let end = (at + 4).min(b.len());
                b[at..end].copy_from_slice(&v.to_le_bytes()[..end - at]);
            }
            4 => b.truncate(at),
            5 => {
                let len = rng.below(64);
                let ins: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
                b.splice(at..at, ins);
            }
            _ => {
                let end = (at + 1 + rng.below(64)).min(b.len());
                b.drain(at..end);
            }
        }
    }
    b
}

fn exercise(bytes: Vec<u8>) -> bool {
    let limits = Limits::default();
    let Ok(mut p) = Package::open(bytes, &limits) else { return false };
    let names: Vec<String> = p.names().map(str::to_owned).collect();
    for n in &names {
        let _ = p.part(n);
        let _ = p.entry_info(n);
        let _ = p.rels_of(n);
    }
    let _ = p.rels_of("");
    let _ = p.save();
    let _ = p.save_rebuilt();
    if let Some(n) = names.iter().find(|n| n.ends_with(".xml"))
        && let Ok(mut d) = p.part(n).map(|c| c.into_owned())
    {
        d.extend_from_slice(b"<!-- q -->");
        let _ = p.set_part(n, d);
    }
    let _ = p.add_media(PNG.to_vec(), "png");
    let _ = p.add_relationship("word/document.xml", "urn:x", "media/x.png", false);
    let _ = p.gc_orphans();
    if let Ok(out) = p.save() {
        // Whatever we write must itself open (or be rejected) without panicking.
        let _ = Package::open(out, &limits);
    }
    true
}

#[test]
fn mutated_zips_never_panic() {
    let iters: usize = std::env::var("QUILLWORK_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(3000);
    let mut seeds = vec![minimal_docx()];
    let mut es = minimal_entries();
    es[2] = es[2].clone().descriptor(true);
    es[3] = es[3].clone().descriptor(false);
    seeds.push(build_zip(&es, b"archive comment"));
    for f in synthetic_corpus() {
        seeds.push(std::fs::read(f).unwrap());
    }
    let mut rng = Rng(0x5157_4C4C_574F_524B);
    let (mut opened, mut total) = (0usize, 0usize);
    for seed in &seeds {
        let hot = hot_spots(seed);
        for i in 0..iters {
            let case = mutate(&mut rng, seed, &hot);
            let len = case.len();
            let r = catch_unwind(AssertUnwindSafe(|| exercise(case.clone())));
            match r {
                Ok(ok) => opened += ok as usize,
                Err(_) => {
                    let dump = std::env::temp_dir().join("quillwork-opc-fuzz-crash.zip");
                    std::fs::write(&dump, &case).ok();
                    panic!("panic on iteration {i} (len {len}); case written to {}", dump.display());
                }
            }
            total += 1;
        }
    }
    println!("fuzz: {total} cases, {opened} opened");
}
