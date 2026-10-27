# quillwork-oracle

Independent checks for Quillwork's output. None of this shares code with the engine: the text oracle is lxml, the
locality oracle is a byte scanner, validity comes from Docxodus and rendering from LibreOffice.

```sh
uv run --project tools/oracle oracle <command> ...      # from the repo root
uv run --project tools/oracle pytest                     # the oracle's own tests
```

| command | what it answers | exit 1 when |
|---|---|---|
| `validate FILE...` | Does the package open and pass Open XML SDK schema + semantic checks? (Docxodus `verify_deliverable`) | any validity error |
| `render FILE [--out DIR] [--against B]` | Does LibreOffice open it, how many pages, and how different do two documents look per page? | LibreOffice can't open it / page counts differ / SSIM below `--min-ssim` |
| `textdiff A [B] [--mode markup\|accept\|reject] [--story PART\|all] [--facts]` | What text does the document say, with tracked changes shown, accepted or rejected? Diff of two. | the two views differ |
| `expect FILE...` | Does a fixture still match its hand-written `.expect.json`? | any mismatch |
| `locality ORIG EDITED -i ID...` | Did the edit touch only the paragraphs it meant to? | any change outside the intended set |
| `tokens FILE... [--raw]` | Token cost of a text file or a document's text view (`--raw`: its document.xml) | never |

All commands take `--json` for machine-readable output.

## validate

Runs Docxodus in a separate `python -I` process (it is a downloaded package and is imported from outside the repo).
It is found via `$QUILLWORK_DOCXODUS` (the directory containing `docx_scalpel/`), else the first
`quillwork-tools-cache/docxodus` next to the repo or one of its parents. `$QUILLWORK_DOCXODUS_PYTHON` picks the
interpreter (default: this venv's, Python 3.12).

Only validity findings count: categories `package`, `openXml`, `relationship`, `structure` at severity error.
`--policy` adds Docxodus's delivery-policy checks (placeholders, editorial markers). `--baseline ORIG` lets Docxodus
mark errors that were already in the original as pre-existing; those don't count. Non-zip files and zips without
`[Content_Types].xml` are rejected up front as `oracle.not_a_zip` / `oracle.no_content_types`.

## render

Converts through LibreOffice in a local [gotenberg](https://gotenberg.dev) container, then rasterises with pdfium
(`pypdfium2`). Start the container once; it listens on loopback only:

```sh
docker run -d --name quillwork-gotenberg -p 127.0.0.1:39300:3000 --read-only \
  --tmpfs /tmp --tmpfs /home/gotenberg gotenberg/gotenberg:8 gotenberg --api-timeout=120s
```

`$QUILLWORK_GOTENBERG` overrides the URL (default `http://127.0.0.1:39300`). From Python:
`render.render(docx, out_dir)` → PNG paths, `render.render_diff(a, b)` → `{"pages_a", "pages_b", "pages": [{"page",
"ssim", "diff_ratio"}], "min_ssim"}` (pages missing on one side score 0). A file LibreOffice can't open raises
`RenderError` with code `libreoffice.open_failed` (gotenberg's HTTP 500); LibreOffice would import non-zip bytes as
plain text, so those are refused first as `oracle.not_a_zip`. LibreOffice in the container occasionally segfaults on a good file, so
a 5xx is retried twice before the file counts as unopenable. Render tests skip when gotenberg isn't reachable.

## textdiff / expect

Three views of every story (main body, headers/footers, footnotes, endnotes, comments, text boxes):

- `markup`: tracked changes inline — `{+ins+}`, `{-del-}`, `{m+moved to+m}`, `{m-moved from-m}`; an inserted or
  deleted paragraph mark shows as `{+¶+}` / `{-¶-}`.
- `accept` / `reject`: the document after accepting / rejecting every change (a removed paragraph mark merges the
  paragraph with the next one; tracked table rows disappear).

Paragraph `index` counts outermost `w:p` elements of a part in document order — table-cell paragraphs count,
text-box paragraphs don't. The locality oracle uses the same numbering. `--facts` adds revision counts and
authors, comments with their anchors, field instructions, bookmarks, content controls, images, tables, notes and
hidden text (vanish, white, ≤2pt, zero-width).

## locality

```sh
oracle locality original.docx edited.docx -i 5 -i 1A2B3C4D -i word/header1.xml:0
```

An intended paragraph is a `w14:paraId` (exactly 8 hex digits; matches in any part), an index into the main part,
or `<part>:<index or paraId>`. The report has:

- `parts`: zip entries added, removed, content-changed, recompressed (same bytes, different compression) and
  whether the entry order changed. Changes to `[Content_Types].xml`, the document rels and the comment parts are
  allowed by default (`--allow GLOB` adds more, `--strict` drops the defaults); any other non-XML or
  paragraph-free part that changed is a violation.
- `paragraphs`: for every changed part with paragraphs, the raw bytes of each outermost `w:p` are aligned between
  the two files (by content, then by paraId inside a changed block). Violations: `paragraph_changed`,
  `paragraph_removed`, `paragraph_inserted` (unless next to an intended paragraph) and
  `markup_between_paragraphs_changed` (anything outside paragraphs: sectPr, table markup, the root element).
- `intended`: per intended paragraph, `changed_chars` (accepted text vs original), `redline_chars` (new tracked
  characters) and `redline_ratio`, `reject_equals_original`, `ppr_preserved`, and `affix_format_lost` (characters
  in the unchanged prefix/suffix whose run formatting changed).

## tokens

`tiktoken` `o200k_base`, local. It is a yardstick for comparing representations, not Claude's tokenizer.
`--anthropic-model M` also calls Anthropic's count_tokens endpoint when `ANTHROPIC_API_KEY` is set — that sends the
text to Anthropic, so never use it on client documents.

## Fixtures

`corpus/fixtures/` is generated by `python3 corpus/synthetic/gen_fixtures.py` (stdlib only, deterministic; a test
fails if the committed files drift from a fresh run). One feature per file, each with a `.expect.json` written by
hand in the generator; `corpus/fixtures/invalid/` holds files that must fail validation, with the expected codes.
