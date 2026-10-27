# Spike result: Rust core — GO (2026-10-26)

DESIGN §2 set five go/no-go criteria for the Rust core. All five are met (binding ergonomics settled with the PyO3 binding, see BINDING.md). Measured on 2 synthetic
files plus 6 private real-world documents (the private corpus; numbers only, files never leave the machine).

| Criterion | Target | Result |
|---|---|---|
| No-op round trip | 100% byte-identical, incl. non-Word producers | 8/8 files byte-identical through `opc::Package::save`; 166/166 XML parts byte-identical through `xml::Document` (32 synthetic + 134 private) |
| Edit locality | one change, nothing else moves | `tests/spike_e2e.rs`: one `w:t` edited → every other zip entry identical, `document.xml` identical outside that element, on all 8 files |
| Speed | at least as fast as lxml (3.9 ms parse / 2.2 ms serialise) | parse 1.10 ms, no-op write 0.012 ms, parse + edit + write 1.15 ms (synthetic doc200, 1.08 MB XML); largest private `document.xml` (812 KB) 1.91 ms parse; open + no-op save of the largest private package 0.034 ms |
| Memory | — | ~10 MB peak for parse + write of the largest XML part; 5.5 MB for open/edit/save of the largest package |
| WASM size | ≤ 200 KB brotli | 97 KB brotli (256 KB raw) for open + parse + edit + save with deflate, `opt-level=z`, no wasm-opt |
| Robustness | no crashes on hostile input | 1,000,000 mutated/truncated zips (release) + 4,000 mutated XML parts: 0 panics; zip bombs, traversal names, duplicates, overlapping entries, encrypted packages, DOCTYPE/entity bombs, 1,000,000-deep nesting all rejected with typed errors |
| Binding ergonomics | judged by writing `replace_text` | GO: `crates/quillwork-py/examples/replace_text.py` does `replace_text` as a tracked change keeping `w:rPr` in ~35 lines of ordinary, `mypy --strict`-clean Python; passes on all 8 files with every other zip entry byte-identical. Clunks (int handles, `text`/`text_content`, qname vs (ns, local)) are binding-only fixes: [BINDING.md](BINDING.md#ergonomics-verdict-design-2-criterion-5-go) |

## Known gaps carried forward

- An edited part is always re-deflated: zlib reproduces none of Word's compressed streams, so the
  edited entry is byte-different (it is smaller) while all other entries are copied verbatim.
- Edited saves don't write zip64 (> 4 GB or > 65,535 entries is an error) and drop junk/gaps between
  entries. Untouched files return their input bytes exactly.
- UTF-16 parts are rejected (UTF-8 only).
- No cargo-fuzz target yet (stable toolchain only); seeded random loops stand in.
