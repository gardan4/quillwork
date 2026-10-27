# Benchmarks

Every comparative number in the README comes from here. All of it runs on documents in this
repository, so anyone can reproduce it.

```sh
pip install python-docx tiktoken      # plus quillwork (a wheel, or `maturin develop`)
python bench/fidelity.py > bench/results.md
python bench/fidelity.py --json results.json corpus/fixtures/04-tables.docx   # one file, raw data
```

The head-to-head against other editing engines (below) has its own script and dependencies; see
[`results-compare.md`](results-compare.md#reproduce).

## What it measures

`fidelity.py` runs each `.docx` in `corpus/synthetic/` and `corpus/fixtures/` through Quillwork
and through [python-docx](https://python-docx.readthedocs.io/), the most widely used Python
library for Word files. python-docx is a document-building library, not an editing engine, so
this is not a contest between peers. It shows what the default tool does to a file it was only
asked to touch lightly.

- **No-op round trip.** Open, save, change nothing.
  - *file*: is the output byte-identical to the input?
  - *entries*: zip entries whose stored bytes (local header plus compressed data) are unchanged.
  - *parts*: entries whose decompressed content is unchanged, so re-compression alone does not count.
- **One-word edit.** Replace one word in one body paragraph with `Edited`. The script picks the
  first word of four or more letters, in the first run of a body paragraph, that occurs once in
  that paragraph and nowhere earlier in the document, so both tools edit the same spot. It
  checks that they did (`same_target` in the JSON). Quillwork writes the edit as a tracked
  change (`replace_text`). python-docx has no tracked-change API, so it overwrites the run text.
  That favours python-docx on output size, not on locality.
  - *other entries changed*: zip entries other than `word/document.xml` whose content changed.
  - *paragraphs changed*: outermost `w:p` elements in `word/document.xml` whose bytes differ
    from the input. One is the minimum: the edited paragraph.
- **Tokens.** `o200k_base` tokens (tiktoken, used as a stand-in for any LLM tokenizer) of:
  - Quillwork's full projection (`Document.read()` with a budget large enough for the whole
    document, with anchors). A default `read()` returns a budgeted first page instead.
  - python-docx's plain paragraph text. It has no anchors, tables, comments or formatting, so
    an agent cannot point at anything in it.
  - The raw `word/document.xml`.

The corpus:

- `corpus/synthetic/doc50.docx` and `doc200.docx` are 50- and 200-page documents that
  `corpus/synthetic/gen.py` builds with python-docx from a random Dutch-like word list.
  Because python-docx wrote them, its own re-serialisation leaves their parts unchanged. Real
  files from Word are different, as the fixtures show.
- `corpus/fixtures/*.docx` are 17 small hand-built packages (`gen_fixtures.py`). Each one covers
  a feature: run formatting, headings, lists, tables, headers and footers, notes, comments,
  tracked changes, images, text boxes, fields, content controls, hidden text, right-to-left,
  CJK, LibreOffice-style markup, and CRLF with a byte-order mark.

## Results

Recorded 2026-10-27 with quillwork 0.0.1, python-docx 1.2.0, tiktoken 0.14.0 and Python 3.12.13
on macOS arm64. The full table is in [`results.md`](results.md).

| | Quillwork | python-docx |
|---|---|---|
| No-op save byte-identical (whole file) | 19 of 19 | 0 of 19 |
| No-op: zip entries with identical stored bytes | all entries, every file | none, every file |
| No-op: parts with identical content (fixtures only) | all | 1 to 5 of 8 to 14 per file |
| Edit: zip entries changed besides `document.xml` | 0, every file | 6 to 12 per fixture (0 on the two python-docx-made files) |
| Edit: paragraphs changed | exactly 1, every file | 1 on 15 files; 2 or 3 on 4 files (tables, fields, LibreOffice-style, CRLF/BOM) |

Tokens: the full projection is 2.9 to 13 times smaller than the raw `document.xml` on the
fixtures, and 1.8 times smaller on the two python-docx-made documents, whose XML is unusually
lean. Plain text is smaller again. It drops the anchors, structure and markup an agent needs to
make a targeted edit, so it is a floor rather than an alternative.

## Head-to-head with other editing engines

`fidelity_compare.py` runs the same corpus through four engines with the same metrics (it imports
them from `fidelity.py`) and the same target word:

- Quillwork;
- [Adeu](https://github.com/dealfluence/adeu), a Python redlining engine built on python-docx;
- [Docxodus](https://github.com/JSv4/Docxodus), a .NET engine, through its Python client
  `docx-scalpel`;
- python-docx, as above.

Each engine makes its edit as a tracked change where it supports one. The script adds two checks
that `fidelity.py` does not make. *Canonical* comparisons repeat each byte-level comparison after
C14N and whitespace normalisation, which separates re-serialisation from real content change.
*Validity* runs the Open XML SDK validator on every output, counting only errors the engine
introduced.

Recorded 2026-10-27 with quillwork 0.0.3, adeu 3.0.7, docx-scalpel 0.9.0 and python-docx 1.2.0.
Method, caveats and per-file tables are in [`results-compare.md`](results-compare.md).

| | Quillwork | Adeu | Docxodus | python-docx |
|---|---|---|---|---|
| Edit: right word changed, no other text changed | 19 of 19 | 19 of 19 | 19 of 19 | 19 of 19 |
| Edit recorded as a tracked change | yes | yes | yes | no (overwrite) |
| No-op: content preserved (canonical XML) | 19 of 19 | 19 of 19 | 19 of 19 | 19 of 19 |
| No-op save byte-identical (whole file) | 19 of 19 | 0 of 19 | 0 of 19 | 0 of 19 |
| Edit: zip entries changed besides `document.xml` | 0 | 2 to 12 | 2 to 8 | 0 to 12 |
| Edit: paragraphs changed (byte-level) | exactly 1 | 1; 2 or 3 on 4 files | 1 to 1,472; every paragraph on 9 files | 1; 2 or 3 on 4 files |
| Edit: new Open XML SDK errors | 0 | 2 per file | 0 | 0 |

All four engines make the correct edit and lose no content on these files. The differences are
at the byte level. The other three engines re-serialise parts they did not change, and Quillwork
writes them back as they were. That matters for diffs, review tools and anything that hashes the
file; it does not change what Word displays. Quillwork's one change outside the edited paragraph
is a namespace declaration on the `document.xml` root, for the `w16du:dateUtc` attribute on its
revisions. Adeu's validator errors come from the same attribute without that declaration. Word
365 knows the attribute, and no Word version was tested against these files. Docxodus's one
canonical change outside the paragraph turns Track Changes on in `settings.xml`.

Adeu's read view takes fewer tokens than Quillwork's (0.23 to 0.99 times on the fixtures). It has
no per-paragraph anchors, because Adeu addresses edits by text match. The corpus is small and
mostly synthetic, and the benchmark was written by Quillwork's author. Corrections are welcome.

## Not measured here

- Rendering. Nothing here checks that Word or LibreOffice draw the output identically. The
  `tools/oracle` harness does that with LibreOffice and an Open XML validator. It needs
  services this script does not install.
- Other engines beyond the three above. safe-docx, Pandoc and "ask an LLM to rewrite the XML"
  are discussed in `docs/DESIGN.md` as design context, with no measured numbers.
- Speed. Engine time is a small fraction of an LLM turn, so it is a budget rather than a goal.
  `crates/quillwork-core/examples/xml_bench.rs` times the XML layer on its own.
