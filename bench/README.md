# Benchmarks

Every comparative number in the README comes from here. All of it runs on documents in this
repository, so anyone can reproduce it.

```sh
pip install python-docx tiktoken      # plus quillwork (a wheel, or `maturin develop`)
python bench/fidelity.py > bench/results.md
python bench/fidelity.py --json results.json corpus/fixtures/04-tables.docx   # one file, raw data
```

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

## Not measured here

- Rendering. Nothing here checks that Word or LibreOffice draw the output identically. The
  `tools/oracle` harness does that with LibreOffice and an Open XML validator. It needs
  services this script does not install.
- Other engines. Docxodus, safe-docx, Pandoc and "ask an LLM to rewrite the XML" are discussed
  in `docs/DESIGN.md` as design context. No reproducible head-to-head numbers for them are
  published yet, so the README makes no claims about them.
- Speed. Engine time is a small fraction of an LLM turn, so it is a budget rather than a goal.
  `crates/quillwork-core/examples/xml_bench.rs` times the XML layer on its own.
