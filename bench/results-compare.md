# Head-to-head fidelity: Quillwork, Adeu, Docxodus, python-docx

Recorded 2026-10-27 on macOS 26.6.2 (Darwin 25.6.0, arm64), Python 3.12.13, with
quillwork 0.0.3, adeu 3.0.7, docx-scalpel 0.9.0 (the PyPI client that bundles the Docxodus .NET host;
the host does not report its own version), python-docx 1.2.0, lxml 6.1.3 and tiktoken 0.14.0.
Raw data: [`results-compare.json`](results-compare.json). Script: [`fidelity_compare.py`](fidelity_compare.py)
(it imports [`fidelity.py`](fidelity.py) for every byte-level metric and for the target-word
picker, so the Quillwork and python-docx columns are measured exactly as in [`results.md`](results.md)).
The per-file tables below are printed by [`compare_report.py`](compare_report.py).

Corpus: the 19 public Quillwork test documents, `corpus/synthetic/*.docx` (2 long documents made with
python-docx) and `corpus/fixtures/*.docx` (17 small hand-built files, one Word feature each).
Every engine ran on every file. Nothing crashed and no file was skipped.

## What each engine did

| | no-op | one-word edit (picked word -> `Edited`) | read view (for tokens) |
|---|---|---|---|
| Quillwork | `Document.open(b).save()` | `apply(replace_text)` at the first `find()` hit: tracked | `read(budget=1e9)` |
| Adeu | `RedlineEngine(b).save_to_stream()` | `process_batch([ModifyText(r"\bword\b", match_mode="first", regex=True)])`: tracked | `extract_text_from_stream` (CriticMarkup) |
| Docxodus | `open_session(b).save()` | `open_session(tracked_changes=RENDER_INLINE)`, `grep(r"\bword\b", max_results=1)`, `replace_match()`: tracked | `project().markdown` |
| python-docx | `Document(b).save()` | overwrite `runs[0].text` of the first paragraph whose first run holds the word: no tracking | paragraph text |

All settings are defaults apart from turning tracked changes on and setting an author.

## Metrics

- **No-op**:
  - whole file byte-identical;
  - zip entries whose stored bytes are identical;
  - parts whose decompressed content is identical.
- **Edit**:
  - zip entries changed besides `word/document.xml`;
  - outermost `w:p` paragraphs changed.

  These are the `bench/fidelity.py` metrics, all byte-level.
- **Canonical** (new here): the same comparisons after canonicalising the XML, to separate
  re-serialisation from content change. Canonicalising means:
  - C14N: inclusive per part, exclusive per paragraph;
  - dropping indentation-only whitespace outside `w:t`-like leaves;
  - sorting the children of `[Content_Types].xml` and `.rels`.

  This removes these differences: the XML declaration, attribute order, quote style, `<x/>` vs
  `<x />` vs `<x></x>`, pretty-printing, entry order, and where namespaces are declared.
- **Target correct**: the picked paragraph's text, with tracked changes accepted, equals the original
  with the word replaced once, and no other paragraph's accepted text changed.
- **Edit kind**:
  - tracked: a new `w:del` holds the word and a new `w:ins` is added;
  - overwrite: no new revision markup.
- **Valid**:
  - every output opens as a zip, every XML part parses, and python-docx loads it;
  - Open XML SDK validation through Docxodus `verify_deliverable` (file format Office2019), with the
    input as baseline. Only errors the engine introduced count, in the categories package, openXml,
    relationship and structure, the same rule as Quillwork's `tools/oracle/validate.py`.
- **Tokens**: o200k_base tokens of each engine's full read view.

## Summary

| | Quillwork | Adeu | Docxodus | python-docx |
|---|---|---|---|---|
| No-op save byte-identical (whole file) | 19 of 19 | 0 of 19 | 0 of 19 | 0 of 19 |
| No-op: zip entries with identical stored bytes | all, every file | none, every file | none, every file | none, every file |
| No-op: parts with identical content (fixtures) | all | 1 of 8 to 14 per file (3 of 10 on images, 0 of 8 on CRLF/BOM) | 5 to 7 of 8 to 14 per file | 1 to 5 of 8 to 14 per file |
| No-op: parts with identical content (2 python-docx-made files) | 17 of 17 | 15 of 17 | 14 of 17 | 17 of 17 |
| No-op: parts canonically identical | all, every file | all, every file | all, every file | all, every file |
| Edit: zip entries changed besides `document.xml` | 0, every file | 6 to 12 per fixture, 2 on the python-docx-made files | 2 on 16 files; 4 to 8 on notes, comments, headers/footers | 6 to 12 per fixture, 0 on the python-docx-made files |
| Edit: ... of which canonically changed | 0 | 0 | 1, every file (`settings.xml` gains `<w:trackRevisions/>`) | 0 |
| Edit: paragraphs changed | exactly 1, every file | 1 on 15 files; 2 or 3 on 4 (tables, fields, LibreOffice-style, CRLF/BOM) | 1 on tables; 2 to 13 on the other fixtures (often all of them); 366 of 853 and 1,472 of 3,391 on the long files | 1 on 15 files; 2 or 3 on the same 4 |
| Edit: paragraphs canonically changed | exactly 1 | exactly 1 | exactly 1 | exactly 1 |
| Edit: right word changed, nothing else's text changed | 19 of 19 | 19 of 19 | 19 of 19 | 19 of 19 |
| Edit: kind | tracked | tracked | tracked | overwrite |
| Zip / XML parse / python-docx load, all outputs | pass | pass | pass | pass |
| New Open XML SDK errors, no-op | 0 | 0 | 0 | 0 |
| New Open XML SDK errors, edit | 0 | 2 on every file | 0 | 0 |

The Quillwork and python-docx numbers match [`results.md`](results.md) (recorded with
quillwork 0.0.1) on every row.

### What the byte-level churn is

Nothing on this corpus is lost semantically: once canonicalised, every engine's no-op is lossless and every
edit changes only the target paragraph. The differences are in how much each engine re-serialises.

- **Adeu** saves through python-docx, so it re-zips and re-serialises every XML part it loads. The
  changes are of these kinds:
  - empty-element form, e.g. `<w:p ...></w:p>` becomes `<w:p .../>` and `<w:rPr></w:rPr>` becomes
    `<w:rPr/>`. That is why the 2 to 3 extra paragraphs change on 4 files, the same paragraphs as
    python-docx;
  - the XML declaration;
  - the order of `[Content_Types].xml` entries;
  - stripped indentation (`fontTable.xml` and `webSettings.xml` on the python-docx-made files);
  - a stripped BOM.

  It also touches the notes, comments and `people.xml` parts on the files that have them.
- **Docxodus** leaves untouched parts alone (5 to 7 of 8 to 14 survive byte-identical) but re-serialises
  `document.xml`, `styles.xml` and `settings.xml`, plus headers/footers, notes and comments when
  present. The changes are:
  - `<w:b/>` becomes `<w:b />`;
  - `utf-8` is lowercased in the declaration;
  - the line break after the declaration is dropped.

  Any paragraph holding an empty element therefore shows as changed. Its tracked mode also switches
  Track Changes on for the document (`<w:trackRevisions/>` in `settings.xml`). That is a real setting
  change, and arguably the desired behaviour, but it is the one canonical change outside the target
  paragraph on any engine.
- **Quillwork** writes back the stored bytes of every untouched entry and splices only the edited
  paragraph into `document.xml`. Its one change outside the paragraph is on the `document.xml` root: it
  declares `w16du` and adds it to `mc:Ignorable`, because its revisions carry `w16du:dateUtc`. The
  paragraph metric does not count this.

### Adeu's Open XML SDK errors

Adeu's `w:ins`/`w:del` carry `w16du:dateUtc` with the namespace declared locally, but `w16du` is not
added to `mc:Ignorable`. The Open XML SDK at Office2019 does not know `w16du` (it is a 2023 namespace),
so it reports `Sch_UndeclaredAttribute` on each `w:ins` and `w:del`. Word 365 knows `w16du`, and
this run did not check whether any Word version actually rejects the file. Quillwork emits the same
attribute and passes because it marks the namespace ignorable. Adeu's no-op outputs validate clean.

### Tokens

| | raw `document.xml` / engine view, fixtures | same, the 2 long files | engine view / Quillwork view |
|---|---|---|---|
| Quillwork `read()` | 2.9 to 13x smaller | 1.8x | 1 |
| Adeu CriticMarkup | 7.1 to 50x smaller | 2.0x | 0.23 to 0.99 (fixtures), 0.90 (long) |
| Docxodus markdown | 2.2 to 22x smaller | 1.3x | 0.44 to 2.3 (fixtures), 1.36 (long) |
| python-docx text | 18 to 224x smaller | 2.1x | n/a |

The views do not hold the same information, so these figures are not a like-for-like efficiency ranking.

- **Quillwork** adds:
  - a document header line;
  - an anchor per block;
  - field codes and content-control metadata;
  - hidden, white and tiny text marked as such (13-hidden-text).
- **Adeu's** view is the most compact. It carries the text of tables, text boxes, image alt text and
  content controls, and comments inline. It has no per-paragraph anchors, because Adeu addresses
  edits by text match. It renders hidden, white and tiny text as ordinary text.
- **Docxodus's** default `project().markdown` adds a 32-hex anchor per paragraph. On these fixtures it
  leaves out:
  - table cell text (it shows `rows: 3, cols: 3`);
  - text-box text;
  - image alt text;
  - the text of content controls.

  Its smaller counts on 04, 09, 10 and 12 come from those omissions. It also renders hidden text as
  ordinary text.
- **python-docx** paragraph text has no tables, notes or headers.

## Caveats

- **Small corpus.** 19 files: 17 small single-feature fixtures plus 2 long documents generated with
  python-docx. None are real-world Word documents. Real files have more feature interaction, and
  results may differ on them.
- **Content loss.** "Content preserved" means canonical-XML equality. It shows nobody lost content on
  these files, not that nobody ever does.
- **What the bytes are.** Most of the byte-level difference is re-serialisation: empty-element
  form, attribute and entry order, the XML declaration and whitespace. It matters for diffs, review
  and anything that hashes parts. It does not change what Word displays.
- **One edit per file.** A single word in a body paragraph, in each engine's default API, with only
  tracked mode turned on. Engine options might reduce churn. None were tuned.
- **Docxodus settings change.** In tracked mode Docxodus also turns Track Changes on in
  `settings.xml`. That is counted as a real change, but it is a deliberate feature.
- **Adeu's SDK errors.** The validator is the Open XML SDK at Office2019 schema level, run through
  Docxodus's own `verify_deliverable`. Adeu's errors come from an unignorable 2023 attribute
  (`w16du:dateUtc`). Word 365 understands it. No Word version was actually tested.
- **Not measured: rendering.** Nothing checked that Word or LibreOffice draw the outputs identically.
- **Not measured: speed.** Docxodus runs a .NET host process.
- **Not measured: read-view quality.** Tokens are reported, but the views contain different
  information. For example, Docxodus's default markdown omits table, text-box and content-control
  text on these files, and only Quillwork marks hidden text.
- **Tracked-change timestamps.** Quillwork and Adeu use a fixed 2026-01-01 timestamp. Docxodus
  stamps the current time. This does not affect any metric.
- **Who wrote this.** The benchmark was written by Quillwork's author, on Quillwork's own test
  corpus. Corrections welcome.

## Per-file results

### No-op (open, save)

Each cell: file / zip entries with identical stored bytes / parts with identical content / parts canonically identical.

| file | Quillwork | Adeu | Docxodus | python-docx |
|---|---|---|---|---|
| doc200.docx | identical / 17/17 / 17/17 / 17/17 | changed / 0/17 / 15/17 / 17/17 | changed / 0/17 / 14/17 / 17/17 | changed / 0/17 / 17/17 / 17/17 |
| doc50.docx | identical / 17/17 / 17/17 / 17/17 | changed / 0/17 / 15/17 / 17/17 | changed / 0/17 / 14/17 / 17/17 | changed / 0/17 / 17/17 / 17/17 |
| 01-run-formatting.docx | identical / 8/8 / 8/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 | changed / 0/8 / 5/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 |
| 02-headings.docx | identical / 8/8 / 8/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 | changed / 0/8 / 5/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 |
| 03-lists.docx | identical / 9/9 / 9/9 / 9/9 | changed / 0/9 / 1/9 / 9/9 | changed / 0/9 / 6/9 / 9/9 | changed / 0/9 / 1/9 / 9/9 |
| 04-tables.docx | identical / 8/8 / 8/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 | changed / 0/8 / 5/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 |
| 05-headers-footers.docx | identical / 14/14 / 14/14 / 14/14 | changed / 0/14 / 1/14 / 14/14 | changed / 0/14 / 5/14 / 14/14 | changed / 0/14 / 1/14 / 14/14 |
| 06-notes.docx | identical / 10/10 / 10/10 / 10/10 | changed / 0/10 / 1/10 / 10/10 | changed / 0/10 / 5/10 / 10/10 | changed / 0/10 / 3/10 / 10/10 |
| 07-comments.docx | identical / 13/13 / 13/13 / 13/13 | changed / 0/13 / 1/13 / 13/13 | changed / 0/13 / 6/13 / 13/13 | changed / 0/13 / 5/13 / 13/13 |
| 08-tracked-changes.docx | identical / 8/8 / 8/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 | changed / 0/8 / 5/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 |
| 09-images.docx | identical / 10/10 / 10/10 / 10/10 | changed / 0/10 / 3/10 / 10/10 | changed / 0/10 / 7/10 / 10/10 | changed / 0/10 / 3/10 / 10/10 |
| 10-textbox.docx | identical / 8/8 / 8/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 | changed / 0/8 / 5/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 |
| 11-fields.docx | identical / 8/8 / 8/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 | changed / 0/8 / 5/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 |
| 12-content-controls.docx | identical / 8/8 / 8/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 | changed / 0/8 / 5/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 |
| 13-hidden-text.docx | identical / 8/8 / 8/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 | changed / 0/8 / 5/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 |
| 14-rtl.docx | identical / 8/8 / 8/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 | changed / 0/8 / 5/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 |
| 15-cjk.docx | identical / 8/8 / 8/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 | changed / 0/8 / 5/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 |
| 16-libreoffice-style.docx | identical / 8/8 / 8/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 | changed / 0/8 / 5/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 |
| 18-crlf-bom.docx | identical / 8/8 / 8/8 / 8/8 | changed / 0/8 / 0/8 / 8/8 | changed / 0/8 / 5/8 / 8/8 | changed / 0/8 / 1/8 / 8/8 |

### One-word edit

Each cell: zip entries changed besides `word/document.xml` (canonically changed) / outermost paragraphs changed of total (canonically changed).

| file | word | Quillwork | Adeu | Docxodus | python-docx |
|---|---|---|---|---|---|
| doc200.docx | Artikel | 0 (0) / 1/3391 (1) | 2 (0) / 1/3391 (1) | 2 (1) / 1472/3391 (1) | 0 (0) / 1/3391 (1) |
| doc50.docx | Artikel | 0 (0) / 1/853 (1) | 2 (0) / 1/853 (1) | 2 (1) / 366/853 (1) | 0 (0) / 1/853 (1) |
| 01-run-formatting.docx | plain | 0 (0) / 1/6 (1) | 6 (0) / 1/6 (1) | 2 (1) / 6/6 (1) | 6 (0) / 1/6 (1) |
| 02-headings.docx | Document | 0 (0) / 1/8 (1) | 6 (0) / 1/8 (1) | 2 (1) / 5/8 (1) | 6 (0) / 1/8 (1) |
| 03-lists.docx | Numbered | 0 (0) / 1/15 (1) | 7 (0) / 1/15 (1) | 2 (1) / 13/15 (1) | 7 (0) / 1/15 (1) |
| 04-tables.docx | Before | 0 (0) / 1/15 (1) | 6 (0) / 3/15 (1) | 2 (1) / 1/15 (1) | 6 (0) / 3/15 (1) |
| 05-headers-footers.docx | Page | 0 (0) / 1/3 (1) | 12 (0) / 1/3 (1) | 8 (1) / 3/3 (1) | 12 (0) / 1/3 (1) |
| 06-notes.docx | sentence | 0 (0) / 1/2 (1) | 8 (0) / 1/2 (1) | 4 (1) / 2/2 (1) | 6 (0) / 1/2 (1) |
| 07-comments.docx | Revenue | 0 (0) / 1/2 (1) | 11 (0) / 1/2 (1) | 6 (1) / 2/2 (1) | 7 (0) / 1/2 (1) |
| 08-tracked-changes.docx | quick | 0 (0) / 1/8 (1) | 6 (0) / 1/8 (1) | 2 (1) / 7/8 (1) | 6 (0) / 1/8 (1) |
| 09-images.docx | inline | 0 (0) / 1/3 (1) | 6 (0) / 1/3 (1) | 2 (1) / 2/3 (1) | 6 (0) / 1/3 (1) |
| 10-textbox.docx | Another | 0 (0) / 1/2 (1) | 6 (0) / 1/2 (1) | 2 (1) / 2/2 (1) | 6 (0) / 1/2 (1) |
| 11-fields.docx | Here | 0 (0) / 1/7 (1) | 6 (0) / 2/7 (1) | 2 (1) / 6/7 (1) | 6 (0) / 2/7 (1) |
| 12-content-controls.docx | Start | 0 (0) / 1/4 (1) | 6 (0) / 1/4 (1) | 2 (1) / 3/4 (1) | 6 (0) / 1/4 (1) |
| 13-hidden-text.docx | Visible | 0 (0) / 1/6 (1) | 6 (0) / 1/6 (1) | 2 (1) / 4/6 (1) | 6 (0) / 1/6 (1) |
| 14-rtl.docx | Left | 0 (0) / 1/4 (1) | 6 (0) / 1/4 (1) | 2 (1) / 4/4 (1) | 6 (0) / 1/4 (1) |
| 15-cjk.docx | Mixed | 0 (0) / 1/4 (1) | 6 (0) / 1/4 (1) | 2 (1) / 4/4 (1) | 6 (0) / 1/4 (1) |
| 16-libreoffice-style.docx | Written | 0 (0) / 1/3 (1) | 6 (0) / 3/3 (1) | 2 (1) / 3/3 (1) | 6 (0) / 3/3 (1) |
| 18-crlf-bom.docx | leading | 0 (0) / 1/3 (1) | 7 (0) / 3/3 (1) | 2 (1) / 3/3 (1) | 6 (0) / 3/3 (1) |

### Edit correctness and validity

Each cell: target edited correctly, edit kind / new Open XML SDK errors after no-op / after edit. Every output of every engine opened as a zip, parsed as XML and loaded in python-docx.

| file | Quillwork | Adeu | Docxodus | python-docx |
|---|---|---|---|---|
| doc200.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| doc50.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 01-run-formatting.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 02-headings.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 03-lists.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 04-tables.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 05-headers-footers.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 06-notes.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 07-comments.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 08-tracked-changes.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 09-images.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 10-textbox.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 11-fields.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 12-content-controls.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 13-hidden-text.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 14-rtl.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 15-cjk.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 16-libreoffice-style.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |
| 18-crlf-bom.docx | yes, tracked / 0 / 0 | yes, tracked / 0 / 2 | yes, tracked / 0 / 0 | yes, overwrite / 0 / 0 |

### Tokens (o200k_base) of each engine's full read view

| file | raw document.xml | Quillwork `read()` | Adeu CriticMarkup | Docxodus markdown | python-docx text |
|---|---|---|---|---|---|
| doc200.docx | 274413 | 152917 | 139661 | 209297 | 134158 |
| doc50.docx | 68939 | 38717 | 34905 | 52663 | 33554 |
| 01-run-formatting.docx | 1509 | 178 | 87 | 252 | 77 |
| 02-headings.docx | 923 | 189 | 50 | 255 | 45 |
| 03-lists.docx | 1663 | 205 | 80 | 468 | 52 |
| 04-tables.docx | 1795 | 189 | 146 | 104 | 8 |
| 05-headers-footers.docx | 771 | 262 | 62 | 353 | 29 |
| 06-notes.docx | 709 | 142 | 73 | 156 | 17 |
| 07-comments.docx | 825 | 150 | 117 | 240 | 19 |
| 08-tracked-changes.docx | 1508 | 126 | 125 | 242 | 25 |
| 09-images.docx | 1389 | 114 | 49 | 107 | 24 |
| 10-textbox.docx | 1339 | 103 | 27 | 62 | 11 |
| 11-fields.docx | 1590 | 182 | 131 | 317 | 28 |
| 12-content-controls.docx | 1166 | 224 | 150 | 98 | 10 |
| 13-hidden-text.docx | 911 | 201 | 50 | 199 | 50 |
| 14-rtl.docx | 961 | 98 | 24 | 125 | 24 |
| 15-cjk.docx | 864 | 98 | 23 | 131 | 23 |
| 16-libreoffice-style.docx | 745 | 97 | 25 | 104 | 25 |
| 18-crlf-bom.docx | 695 | 98 | 23 | 102 | 24 |

## Reproduce

From the repository root, in a fresh environment (Docxodus needs no separate install: the
`docx-scalpel` wheel bundles its .NET host):

```sh
uv venv --python 3.12 .venv
uv pip install --python .venv quillwork==0.0.3 adeu==3.0.7 docx-scalpel==0.9.0 python-docx==1.2.0 lxml tiktoken structlog
.venv/bin/python bench/fidelity_compare.py       # writes bench/results-compare.json (about 15 s)
.venv/bin/python bench/compare_report.py         # prints the per-file tables
```

`--no-validate` skips the Open XML SDK pass; pass file paths to run a subset.
