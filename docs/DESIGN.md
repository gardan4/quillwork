# Document engine: design

Status: the original design record, written 2026-10-26 before any code. It records intent; where it and the
code disagree, the code and the other files in `docs/` win. Some numbers below were measured on a private set
of real-world HR documents that cannot be published; they are marked "(private set)" and are not
reproducible from this repo. Reproducible measurements live in `bench/`.

## 1. What we are building

An engine and service that lets an AI agent edit real Office files, starting with Word, the way a careful
colleague would:

- The agent reads the document.
- It makes small, targeted edits as **native Word tracked changes**.
- It leaves comments where needed.
- Everything it did not touch survives **byte for byte**.

People watch the edits appear live in **our own viewer** next to the chat. They accept or reject each change,
then export the **exact** `.docx` (with or without the redlines) or a PDF.

It is built as a stand-alone package that any host application can embed.

### The one rule everything follows

> **The Office file is the document. Everything else is a view of it.**

We never convert a document into our own format and back. The viewer, the text the AI reads and the search
index are all **projections**, derived from the file and thrown away when it changes. Edits are applied to the
file's own XML. This is why a client's letterhead, numbering, fonts, fields and the odd things we don't
understand all survive. It is also what separates an engine that leaves untouched paragraphs byte-identical
from one that normalises them on the way through.

### Goals

1. **Fidelity:** a load and save with no edits gives back the whole file byte-identical. Edited parts change only
   where we edited.
2. **Word-native review:** every AI change is a real `w:ins` / `w:del` / `w:rPrChange` / … that Word, Google
   Docs and LibreOffice show and can accept or reject.
3. **AI-shaped API:** a compact text projection with stable anchors, and a small set of focused operations.
   Each operation is checked against what the agent expected to be there.
4. **Live viewer:** a fast, good-looking, non-paginated reading view with images, tables, comments and
   changes, updated by patches as the agent works.
5. **Exact export:** DOCX as stored; PDF rendered by LibreOffice from that same DOCX.
6. **Safe on hostile files:** zip bombs, XXE, external links, macros, and prompt injection hidden in the
   document.
7. **Packageable:** a language-neutral spec, a Rust core with Python and WASM bindings, a TypeScript viewer,
   an MCP server and a reference HTTP service, Apache-2.0.

### What we optimise for, in order

Speed and size are **budgets to stay under, not goals to push**. One agent edit turn takes 2.5 s (Sonnet 5.5,
low effort) to 30 s (reasoning on). The engine's share is a few milliseconds: on a 46-page real-world document (private set),
lxml parses `document.xml` in 4 ms and one tracked replace takes 0.3 ms. That is under 1% of the turn. Speed
only matters where a person is watching: an edit appearing in the viewer, or typing in it.

What does decide cost and quality is everything below, in this order. When two goals conflict, the higher one
wins.

| # | Goal | Measurable target |
|---|---|---|
| 1 | **Fidelity** | A no-op save is byte-identical (whole file) on 100% of the corpus, including files from non-Word producers. Zero changed bytes outside the edited paragraphs. 100% open in Word with no repair prompt and pass the Open XML SDK validator. |
| 2 | **Safety on hostile files** | 0 crashes or hangs over 24 h of fuzzing. Hard limits enforced (§12). 100% of the hidden-text injection set is marked in the projection. |
| 3 | **Edit quality** | Redline size ≤ 1.2× the characters that actually changed. 100% of P0 ops keep `pPr`/`rPr`. Blind reviewers prefer our redlines at least as often as Docxodus's on a real-world evaluation set. |
| 4 | **Tokens and agent ergonomics** | ≤ 550 tokens per page of projection (≤ 1.25× plain Markdown). Anchor overhead ≤ 10%. First call ≤ 8k tokens. Median ≤ 1.5k tokens returned per edit turn. Re-read rate (window reads per successful op) ≤ 10%. First-try op success ≥ 95%. All tool schemas together ≤ 2k tokens. |
| 5 | **Determinism** | Same input + same ops + injected clock and id source → byte-identical output, 100% of runs. Needed for golden tests, caching and reproducing bugs. |
| 6 | **Generality of hosts and models** | One engine for Python, Node and the browser (via WASM); CLI, MCP and HTTP from v0.1. One schema set that passes OpenAI strict mode, Anthropic and MCP validation. RTL and CJK in the data model from day one. |
| 7 | Interactive speed *(budget)* | Engine ≤ 30 ms per op and op → painted viewer patch ≤ 100 ms p95 on a 200-page document. Open to first paint ≤ 1.5 s. PDF ≤ 5 s p95, cached by content hash. |
| 8 | Observability | Every op emits a structured event (op, anchor, result code, tokens) and an OpenTelemetry span: the only way to track re-read rate and first-try success. |
| 9 | Size *(budget)* | Viewer core ≤ 60 KB gzip. Engine wheel ≤ 1 MB per platform, WASM ≤ 200 KB brotli. Engine image ≤ 150 MB; the PDF sidecar no bigger than `gotenberg:8-libreoffice` (440 MB compressed). ≤ 40 MB RSS per open 200-page document. Output `.docx` never bigger than needed (§3). |
| 10 | Longevity | Few runtime dependencies, a feature matrix generated from the corpus, at least two maintainers before going public. |

**Tokens are the biggest lever.** Measured on that 46-page document (private set; tiktoken `o200k_base` as a stand-in for
Claude's tokenizer, so treat the absolute numbers as ±25%; phase 0 re-counts with Anthropic's `count_tokens`):

| Representation | Tokens per page | vs plain lines |
|---|---|---|
| Markdown, no anchors | 398 | 0.89× |
| Line projection, no anchors | 447 | 1.00× |
| + base32 block handles (§5.1) | 500 | 1.12× |
| + hex `paraId` anchors (the first draft) | 527–547 | 1.18–1.22× |
| HTML with ids | 602 | 1.35× |
| JSON (compact) | 820 | 1.83× |
| Raw `document.xml` | 7 383 | 16.5× |

Re-sending a 200-page projection uncached costs about $0.20 per turn on Sonnet 5.5; outline plus a window,
then diffs only, costs about $0.003. §5.2 and §6.1 are built around that.

**Generality means breadth of hosts, not formats.** Word first, PowerPoint next. Excel is a different problem
(cells, formulas, recalculation) and is not promised. Projects that went broad on formats (SheetJS, ExcelJS)
stalled; the long-lived ones (Typst, mammoth) are narrow and deep.

### Non-goals (deliberately)

- **A layout engine or pixel-perfect pagination.** Word itself doesn't paginate identically across machines.
  When people need "how it will print", we show LibreOffice's PDF (§9.3).
- **A full WYSIWYG word processor.** Users edit by chat plus light inline text edits. For heavy manual work,
  "Open in Word" and re-upload is supported, and we diff it back (§11.3).
- **Real-time multi-user co-editing (CRDTs).** One writer per document at a time; edits are serialised on the
  server.
- **Legacy `.doc` / `.rtf` / `.odt` as first-class formats.** They are converted to `.docx` on import, with a
  notice.

## 2. Architecture

```
                         ┌──────────────────────────────────────────────┐
  spec/  (language-      │  op schemas · view-model schema · anchor     │  ← the contract; JSON Schema,
  neutral contract)      │  grammar · projection grammar · error codes  │    versioned, tested
                         └──────────────────────────────────────────────┘
 ┌──────────────────────────── quillwork-core (Rust) ────────────────────────────────┐
 │  opc/        package: safe zip, raw-entry splicer, [Content_Types], rels, parts    │
 │  xml/        raw-span token tree on quick-xml, namespaces, mc:AlternateContent     │
 │  wml/        Word model = typed *views over the XML tree* (no separate AST)       │
 │    stories     body · headers · footers · footnotes · endnotes · comments · textboxes│
 │    styles      full property resolution (docDefaults → table → numbering → para → run)│
 │    numbering   list label computation ("3.2.a"), restarts, continuations          │
 │    drawings    inline/anchored images, charts, SmartArt, shapes, VML               │
 │    fields      simple + complex fields, cached results                              │
 │  anchors/    stable block + inline addressing                                      │
 │  project/    → LLM text projection   → view model (JSON) + patches                │
 │  ops/        edit operations (pure functions on the tree, transactional)          │
 │  track/      tracked-change writer · word-level diff · accept/reject · compare     │
 │  validate/   invariants (always on) · XSD (optional) · external oracles (CI)       │
 │  assets/     media extract/insert/dedupe · format conversion hooks               │
 │  export/     docx (redline / clean / original) · pdf via converter hook           │
 │  lint/       document-level checks after an edit (cross-refs, numbering, terms)   │
 └─────────────────────────────────────────────────────────────────────────────────────┘
   bindings:  quillwork (Python, PyO3 + maturin, abi3)  ·  @quillwork/core-wasm (wasm-bindgen)
 ┌── quillwork-mcp ──┐  ┌── quillwork-server ─────────────┐  ┌── @quillwork/viewer (TS) ──┐
 │ MCP tools from    │  │ reference FastAPI service:       │  │ framework-free renderer of │
 │ the op schemas    │  │ store, versions, SSE patches,    │  │ the view model + React     │
 │ (any agent)       │  │ assets, export, PDF (Gotenberg)  │  │ wrapper; review UI         │
 └───────────────────┘  └──────────────────────────────────┘  └────────────────────────────┘
                 ↑ a host application plugs in here: auth, storage, agent tools, chat, billing
```

### Language: Rust core, Python and WASM bindings, TypeScript viewer, JSON Schema contract

We chose Rust for **fidelity and reach, not speed**. Speed is a budget the engine meets in any language (§1).
What Rust buys:

- **Byte-exact round trips, including files Word didn't write.** A prototype measured on six evaluation
  documents (private set; 18 XML parts, no edits, part compared byte for byte after a round trip):

  | Engine | Parts identical | Biggest `document.xml` parse / serialise | Peak memory |
  |---|---|---|---|
  | quick-xml event stream re-emitted (Rust) | **18/18** | 0.8 / 0.3 ms | 12 MB |
  | lxml (Python) | 15/18 after fixing up the declaration | 3.9 / 2.2 ms | 59 MB |
  | xot (Rust DOM) | 15/18, and it writes raw tabs/newlines into attributes and turns CDATA into text | 10.2 / 9.5 ms | 42 MB |

  lxml's three misses are all in the one document a non-Word tool produced (it reorders `mc:Ignorable`
  namespace declarations). Those are exactly the files a "never rewrite what we didn't touch" engine must get
  right.
- **One engine everywhere.** The same core runs in Python (PyO3), Node and the browser (WASM). The browser can
  then do instant local edits and previews, and other languages get the real engine, not a port.
- **Small and dependency-light.** Native core about 0.5 MB; PyO3 wheel 0.3–0.6 MB (lxml alone is 4.3 MB);
  WASM 107–150 KB brotli. FFI overhead per call is negligible next to the work.

**How the XML layer works: a raw-span token tree.** No Rust crate keeps a mutable tree *and* the source bytes,
so we build a small one on `quick-xml`:

- Parse into an arena of nodes. Each node keeps its **source byte span** and a **dirty** flag.
- Clean subtrees are re-emitted by copying their original bytes verbatim; only dirty nodes are serialised.
- Namespace scoping, attribute order and prefixes come from the source; no XPath (we walk the tree).
- The Python and JS APIs are **handle-based** (document handle + node ids), because PyO3 classes can't hold
  borrows into the arena.

Estimated at 2–4 engineer-weeks on its own (unverified until the spike).

**Crate stack** (all permissive):

| Crate | Version | Licence | Use |
|---|---|---|---|
| quick-xml | 0.42.0 | MIT | tokenizer under the token tree |
| zip | 9.1.2 | MIT | reading entries; `default-features = false`, `features = ["deflate-flate2-zlib-rs"]` (pure Rust deflate) |
| pyo3 + maturin | 0.30.1 / 1.15 | Apache-2.0 / MIT | Python wheel, `abi3-py312` (one wheel per platform; raised from py310, see BINDING.md) |
| wasm-bindgen | 0.2.129 | MIT / Apache-2.0 | WASM build for the browser and Node |

Node uses the WASM build at first; a native napi binding comes only if a host needs it.

**What stays in other languages:**

- **Python:** the MCP server, the reference HTTP service, the conformance harness, and host
  adapters. They call the core through the wheel.
- **TypeScript:** the viewer. It *reads* the view model and never touches OOXML. It may load the WASM core
  for local previews, but works without it.
- **The contract is JSON Schema in `spec/`:** the op schemas, the view model, the anchor grammar and the
  projection grammar. Bindings and other engines are tested against the same conformance corpus.

### The 2-week spike, and what happens if it fails

Rust is decided by a time-boxed spike at the start of phase 0 (§17), not up front.

**In scope:**

1. Zip splicer with the bomb limits (§3, §12).
2. Raw-span token tree: parse, namespace scoping, dirty tracking, a mutation API, serialise.
3. Text and block index over the body (enough to find a paragraph by text and give it a handle).
4. One real op through the Python binding: `replace_text` as a tracked change, keeping `rPr`.
5. WASM build of the same core.
6. The corpus identity suite and a first fuzz run.

**Go** if all of these hold:

- 100% byte-identical no-op round trip on the corpus, including the non-Word-producer file.
- At least as fast as lxml on the biggest document; memory no worse.
- WASM ≤ 200 KB brotli.
- No crash or hang in the fuzz run.
- The binding API feels ordinary to write ops against from Python (judged by writing `replace_text` with it).

**No-go:** fall back to the first draft of this design: a Python core on `lxml`, with the zip splicer and the
anchor and diff design unchanged. The spec makes a later port possible. We do **not** run two engines side by
side long term.

Rejected:

- **.NET (like Docxodus):** a 177 MB native host in our Python stack, and a smaller contributor pool for us.
- **DOM crates (xot, minidom, xmltree, xmlem, sxd-document; roxmltree is read-only):** none keeps source bytes,
  and xot, the best of them, lost to lxml on fidelity. minidom is also MPL and panics on bad input.
- **Existing Rust OOXML crates (docx-rs, docx-rust, ooxmlsdk, umya):** they parse into a model and regenerate
  the XML. ooxmlsdk 0.14 round-tripped 0 of the 18 parts above identically (private set). Regenerating the XML
  rewrites parts the edit never touched.

## 3. Package layer (`opc/`, `xml/`)

| Concern | Design |
|---|---|
| Load | Read the zip **once** into a `Package`: the original file bytes plus, per entry, the byte range of its local header and compressed data, its compression method and level, flags and timestamps. Parts are inflated and parsed **lazily**, only when something asks for them. |
| Save | A no-op save returns the **original file bytes**. Otherwise our own splicer (about 200–300 lines) copies every clean entry's local header and compressed data **verbatim**, re-deflates only dirty parts (keeping each entry's method and level), and rewrites the central directory. Entry order and names are kept. We don't use a zip library's writer: Python's `zipfile` recompresses every entry (a level-6 re-zip of a real-world document changed every entry and its size), and the Rust `zip` crate's raw copy keeps entry data but still rewrites version-made-by and flags. |
| Size | New media is **deduplicated by content hash** (the same logo inserted twice is stored once). Garbage collection removes only parts and relationships **our own edits** orphaned, never pre-existing ones. Anchor ids (`w14:paraId`) are only stamped on paragraphs we edit, never on untouched ones (stamping them all grew a test file 15%). Media is ~80% of a typical file, so the output is never meaningfully bigger than the input. |
| Serialise | The raw-span token tree (§2) re-emits clean subtrees as their original bytes and serialises only dirty nodes. The original XML declaration is kept. No pretty printing, so untouched elements inside a dirty part come out unchanged. Tested byte for byte per paragraph on the corpus. |
| Content types | `[Content_Types].xml` is read into Default (by extension) and Override (by part) maps. New parts register an Override, or rely on a Default for media. |
| Relationships | `RelGraph` per source part: id → (type, target, mode). New ids are `rIdQw<n>`, so they can't collide. External targets are flagged and **never fetched** (§12). |
| Unknown content | Unknown parts, unknown elements and attributes, and `mc:AlternateContent` choices are left alone. We edit the `mc:Choice` / `mc:Fallback` pair *together*, or not at all (images and text boxes have both). `mc:Ignorable` prefixes are never stripped. |
| Strict OOXML | Detected on load (`http://purl.oclc.org/ooxml/...` namespaces). v1 is read-only for Strict and converts via LibreOffice to Transitional on request. Rare in practice. |
| Encrypted / IRM | An `EncryptedPackage` OLE container, or rights-managed content, is rejected with a clear error ("remove protection in Word first"). Sensitivity labels (MIP, in `docProps/custom.xml`) are **preserved and surfaced** in the view model, so the host can apply policy. |
| Macros (`.docm`) | Load and preserve `vbaProject.bin` untouched. Never executed, never edited. Flagged in metadata. |

## 4. The Word model (`wml/`)

### 4.1 Views over the tree, not an AST

Each concept is a thin typed view over a node in the token tree: `Paragraph`, `Run`, `Table`, `Row`, `Cell`,
`Drawing`, `Field`, `Comment`, `Revision` and so on. A view reads and writes the node in place. In Rust a view is
a node id plus a borrow of the arena; Python and JS get handle-based wrappers (document handle + node id).

There is no second copy of the document, so nothing can fall out of sync, and anything we don't model is
still sitting in the tree when we save.

### 4.2 Stories

A **story** is any independent stream of block content. The engine treats them uniformly, so every op works in
every story:

| Story | Part | Anchor prefix |
|---|---|---|
| Main body | `word/document.xml` | `b` |
| Headers / footers (default, first, even, per section) | `word/headerN.xml`, `word/footerN.xml` | `h1`, `f1`, … |
| Footnotes / endnotes | `word/footnotes.xml`, `word/endnotes.xml` (ids −1/0 are separators, hidden) | `fn3`, `en1` |
| Comments | `word/comments.xml` (+ extended/ids/extensible, `people.xml`) | `c5` |
| Text boxes | `w:txbxContent` inside a drawing (in **both** the `mc:Choice` and the VML fallback) | `tb2` |
| Glossary (building blocks) | `word/glossary/document.xml` | preserved only |

### 4.3 Style resolution

Both the viewer (how things look) and the editor (how new text should look) need **effective** properties. We
implement the full ECMA cascade:

```
docDefaults (rPrDefault/pPrDefault)
 → table style (incl. conditional formats: firstRow, lastCol, band1Horz…, gated by tblLook)
 → numbering level pPr/rPr
 → paragraph style (basedOn chain; linked char style)
 → run (character) style (basedOn chain)
 → direct formatting
```

The cascade also handles:

- **Toggle properties** (`b`, `i`, `caps`, `smallCaps`, `strike`, `dstrike`, `vanish`, …): XOR along the style
  chain, overridden by a direct setting.
- **Theme fonts and colours** (`asciiTheme`, `themeColor` + tint/shade) from `word/theme/theme1.xml`.
- **Complex-script and East Asian variants** (`rFonts/@cs`, `@eastAsia`, `bCs`, `szCs`) chosen by script.

The result is cached per paragraph or run and invalidated when that element, or a style it depends on, is
touched.

### 4.4 Numbering

`numbering.xml`: `w:num` → `w:abstractNum` (+ `lvlOverride`/`startOverride`).

- We compute the **displayed label** for every numbered paragraph ("3.2", "b)", "•"). This means walking the
  document in order with per-`numId` counters, applying `lvlRestart`, `isLgl`, `suff` and `lvlText`.
- The labels go into both projections, because "Article 3.2" is how both humans and the AI refer to text.
- Numbering on styles (`w:pStyle` → style `w:numPr`) counts too, which is how most legal templates number
  headings.
- Ops can continue a list, restart it, change its level, or add or remove numbering. The rules for each are in
  §6.3.

### 4.5 Fields

- **Simple fields** (`w:fldSimple`) and **complex fields** (`fldChar` begin → `instrText` → separate → result →
  end, which may span runs and even paragraphs) are parsed into `Field` objects: instruction, switches, cached
  result range.
- **The projection shows the cached result** (what Word displays), marked as a field, so the AI doesn't edit a
  page number as if it were text.
- **Editable fields:** `MERGEFIELD`, `REF` (the cached text), `HYPERLINK`. Everything else is read-only text in
  the projection.
- **New `PAGE` / `NUMPAGES` / `TOC` / `REF` fields** are written as complex fields. They are marked `w:dirty`
  only when strictly needed; never `updateFields` in settings, because Word then nags the user with a dialog.
  The PDF export recomputes fields through LibreOffice.

## 5. Anchors and projections

### 5.1 Anchors: stable addresses an LLM can use

The AI must be able to say *which* paragraph without counting. Anchors must survive edits elsewhere. Content
hashes (Docxodus) fail that test: the anchor of a paragraph changes when the paragraph changes.

- **Block anchor = a short handle: story letter + base32 serial**, e.g. `b1dk` (body), `h1-02` (header 1),
  `fn3-1` (footnote 3). A story with a number takes a dash before the serial.
  - The serial is **Crockford base32, lowercase** (`0-9a-z` without `i l o u`), so it can't be misread and
    never looks like an article number. Three characters give 32,768 handles per story; when a story runs
    out, *new* handles get four characters and old ones keep theirs.
  - Handles are minted in document order on first load, **never reused**, and new paragraphs get the next
    free serial. They don't renumber when the document changes.
  - A **sidecar map** stores handle → `w14:paraId` (or, for a paragraph without one, its position and a text
    fingerprint). It is persisted with the document version, so handles stay the same across sessions.
  - The first time we *edit* a paragraph that has no `w14:paraId`, we stamp a fresh one (`< 0x80000000`,
    unique). Word accepts and keeps these. Untouched paragraphs are never stamped (§3).
  - Why not the paraId itself: an 8-character hex id costs about 4.8 tokens against 2.2 for a handle, and a
    "shortest unique prefix" (the first draft) gets longer as the document grows, so an anchor the agent read
    earlier can stop being valid. Bare integers (1.5 tokens) were rejected because they clash with article
    and list numbers ("edit 3" vs "article 3"). UUIDs cost 22.6 tokens each.
- **Table addressing:** tables have no paraId of their own, so each table gets an id `t<n>`, kept in the
  sidecar map and re-found after an outside edit by its first cell's paraId. Cells are `t2.r2.c3`. Cells
  contain blocks, so a paragraph inside a cell also has its own paragraph anchor.
- **Anchors are versioned.** Every read returns the `version` it was taken at. Ops quote that version, and the
  error on a mismatch carries the fresh text, so the agent re-reads only that block. SuperDoc's agent API
  works the same way: its targets are tied to a document revision.
- **Inline addressing is by text, not offsets:** `{anchor, find: "38 uur", occurrence: 1}`. LLMs are reliable
  at quoting and unreliable at counting characters. If `find` is ambiguous and no occurrence is given, the op
  is rejected with the candidates listed.
- **Re-identification after an outside edit** (the user edited in Word and re-uploaded): first match on paraId,
  then on content similarity plus position (§11.3).
- **Guards:** every mutating op carries `expect` (the block's current text, or a hash of it) and
  `base_version`. A mismatch is rejected with `STALE_ANCHOR`, and the error includes the current block text so
  the agent can retry with no extra read. This generalises the `before` check of an earlier prototype.

### 5.2 LLM projection (text)

The projection is compact, line-oriented and Markdown-flavoured. One block per line, the anchor first:

```
§ doc "Arbeidsovereenkomst — J. de Vries" · 14 pages (est.) · nl-NL · tracked changes: on · 3 open comments
§ outline: [b1az] 1 Partijen · [b1c8] 2 Functie · [b1dk] 3 Arbeidsduur · …

[b1az H1 #1] Partijen
[b1b3 P] De ondertekenden: **Voorbeeld B.V.**, gevestigd te Utrecht, hierna "werkgever", en
[b1dk H1 #3] Arbeidsduur
[b1dm LI #3.1] De werknemer werkt {--40--}{++38++}⟨r12,13 AI⟩ uur per week. {>>c5<<}
[t2 TABLE 3×4 "Salarisschaal"]
  [t2.r1] Schaal | Min | Max | Periodiek
  [t2.r2] 7 | € 3.012 | € 4.180 | 2,5%
[img4 IMAGE inline 6.2×3.1cm alt="Logo Voorbeeld"]
[b1fr P] Zie artikel ⟪REF _Ref123 → "3.1"⟫ voor de werktijden.
```

Its features:

- **Two modes:**
  - `accepted` (the default for the AI: reads clean, as if all changes were accepted, with markers only on
    changes in its own open change set);
  - `markup` (shows every `{++ins++}`, `{--del--}` and `⟨fmt: +bold⟩`, with author and id).

  The notation is **CriticMarkup**: models already know it, and Adeu uses it for the same job. The AI
  writes plain text in its ops; CriticMarkup appears only in what it reads.
- **Semantic appendix** (an idea taken from Adeu): after the text come the defined terms with the anchor of
  their definition (`"Werkgever" → b1b3`), the cross-references (`REF _Ref123 in b1fr → b1dm "3.1"`) and
  the content controls and placeholders. It is cheap to compute and stops the most common legal-drafting
  mistakes before they happen.
- **Paging for long documents:**
  - `outline` lists the headings and their anchors;
  - `read(range=b1dk..b1fr)` and `find(regex)` return the anchors of matching blocks.
  - There is a token budget per call (default 8k tokens). The first call returns header, outline and the first
    window.
  - For a document-wide instruction ("make every 'werknemer' gender-neutral"), the agent loop visits each
    window in turn rather than reading the whole document at once. Models attend worst to the middle of a
    long context; one window at a time keeps every part of the document near the front.
- **Diffs, not re-reads.** After the first call, the agent only ever receives what changed:
  - every op returns the changed blocks (in this projection) and the new `version`, nothing else;
  - `STALE_ANCHOR` and `AMBIGUOUS` errors carry the current text of the block or the candidates, so a failed
    op never needs a separate read;
  - the system prompt, tool schemas and outline form a **stable prefix** that doesn't change between turns,
    so the model provider's prompt cache hits on it. The outline is only re-sent when headings change.

  On a 200-page document that is the difference between about $0.20 per turn (whole projection re-sent),
  $0.01 (re-sent but cached) and $0.003 (outline + window + diffs) on Sonnet 5.5.
- **Few, small tools.** At most 6–8 tools exposed to the agent at once, with all schemas together under
  2k tokens. Rarely used ops sit behind one `advanced_op` tool rather than each having its own schema.
- **Untrusted-content markers:** hidden text (`w:vanish`), white-on-white text, text under 2pt, and text in
  `w:del` from other authors are **shown as marked** (`⟦hidden⟧ …`) and never silently mixed into the text.
  This is a prompt-injection defence (§12).
- **Inline marks:** `**bold**`, `*italic*`, `__underline__`, `^sup^`, `~sub~`. Only marks that differ from the
  paragraph's resolved style are shown, so the text stays readable.

### 5.3 View model (JSON) for the viewer

The view model is a resolved, render-ready tree. The viewer never sees XML.

```jsonc
{
  "version": 41,
  "meta": { "title": "...", "lang": "nl-NL", "trackChanges": true, "labels": ["Confidential"] },
  "sections": [{ "id": "s1", "page": { "w": 11906, "h": 16838, "margins": {...}, "cols": 1 },
                  "header": "h1", "footer": "f1", "titlePage": false }],
  "blocks": [
    { "id": "b1dm", "kind": "p", "section": "s1", "style": "ListParagraph",
      "list": { "label": "3.1", "level": 0 },
      "fmt": { "align": "both", "indent": {...}, "spacing": {...}, "font": "Calibri", "size": 22 },
      "inlines": [
        { "t": "De werknemer werkt " },
        { "t": "40", "rev": { "id": 12, "type": "del", "author": "AI assistant", "cs": "cs_7" } },
        { "t": "38", "rev": { "id": 13, "type": "ins", "author": "AI assistant", "cs": "cs_7" } },
        { "t": " uur per week.", "comments": ["c5"] }
      ]},
    { "id": "img4", "kind": "image", "asset": "a_9f2c…", "w": 2232000, "h": 1116000,
      "wrap": "inline", "alt": "Logo Voorbeeld", "crop": null, "rotation": 0 },
    { "id": "t2", "kind": "table", "grid": [...], "rows": [{ "cells": [{ "span": 1, "vmerge": null,
      "shade": "#F2F2F2", "blocks": [...] }] }] }
  ],
  "comments": [{ "id": "c5", "author": "...", "date": "...", "anchor": {...}, "replies": [...], "done": false }],
  "changeSets": [{ "id": "cs_7", "label": "Werkweek naar 38 uur", "author": "AI assistant",
                   "turn": "turn_123", "revisions": [12, 13], "status": "open" }]
}
```

- **Patches:** after each op, the server emits `{version, upserts: [block…], removes: [id…], moves: [...],
  changeSets, comments}`, and the viewer applies them by block id. This is what makes edits "appear live".
- Fonts are mapped to metric-compatible web fonts: Calibri → Carlito, Cambria → Caladea, Arial → Arimo,
  Times New Roman → Tinos, Courier New → Cousine. Embedded document fonts are served when present.

## 6. Edit operations (`ops/`)

### 6.1 Principles

1. **Small and named.** Each op does one thing a reviewer understands ("replace '40' with '38' in 3.1"). No
   "rewrite the whole section" op exists; rewriting is many small replaces, which gives a readable redline.
2. **Transactions.** `apply([op…])` is atomic. On any error (guard failure, validation failure) nothing changes.
   One transaction is one **change set**: the unit the UI shows as a card with Accept/Reject, and the unit
   linked to a chat turn.
3. **Guards.** `expect` + `base_version` on every mutating op (§5.1).
4. **Formatting is inherited, never invented.** New text takes the formatting of its neighbours; new paragraphs
   clone the anchor paragraph's `pPr`. This fixes both Docxodus gaps.
5. **Tracked by default.** `mode: "track" | "direct"`. Direct mode is for documents the AI creates from scratch
   (no one to review against yet) and for accepting or rejecting changes.
6. **Every op returns a diff, never the document.** The agent gets the changed blocks in projection form plus
   the new `version`; the viewer gets the view-model patch (§5.3). Errors carry the fresh text (§5.2). We
   track the cost of this per turn: tokens returned, re-read rate, first-try success, `STALE_ANCHOR` and
   `AMBIGUOUS` rates, and prompt-cache hit ratio (targets in §1).

### 6.2 Op catalogue

**Text**

| Op | Notes |
|---|---|
| `replace_text {anchor, find, replace, occurrence?}` | Word-level diff (§7.2); keeps each surviving token's run formatting; inserted words take the left neighbour's `rPr` (Word's own rule). |
| `insert_paragraph {after\|before: anchor, text, style?: "inherit"\|<styleId>, list?: "continue"\|"none"\|{level}}` | Clones the anchor's `pPr` (minus `sectPr`, `pPrChange`, `framePr`, and `numPr` if `list:"none"`); the run `rPr` is the anchor's *last-run* formatting minus character-specific marks. `text` may use the inline marks of §5.2. Multi-paragraph text gives multiple paragraphs. A new paragraph that would end its container or sit right before a table has its insertion carried by the previous paragraph's mark (§7.3). |
| `delete_block {anchor}` | Tracked: every run's `w:t`→`w:delText` (inside `w:del`), `w:instrText`→`w:delInstrText`, and the paragraph mark deleted via `w:pPr/w:rPr/w:del`. A mark that can't merge (the **last** paragraph of its container, or one right before a table or block content control) is never tracked: we delete the paragraph's runs and mark a *previous* paragraph's mark instead (§7.3, "Marks that can't merge"). A paragraph carrying a section's `sectPr` hands the `sectPr` to its neighbour. |
| `move_block {anchor, after\|before}` | v1 writes a tracked delete plus a tracked insert, which every consumer handles. Real tracked moves (`w:moveFrom`/`w:moveTo` + range markers with a paired `w:name`) are P1; they are fragile, and LibreOffice and pandoc show them differently. |
| `split_paragraph {anchor, at_text}` / `merge_paragraphs {first, second}` | Paragraph-mark insert / delete. |
| `set_text_format {anchor, find?, marks: {bold:true,…}}` | `w:rPrChange` with the old `rPr` snapshot. |
| `set_paragraph {anchor, style?, align?, indent?, spacing?, keep_with_next?, page_break_before?}` | `w:pPrChange`. |
| `insert_break {after, kind: page\|column\|section(next\|continuous)}` | A section break copies the current `sectPr`; tracked via `sectPrChange` where applicable. |
| `insert_footnote {anchor, find, text}` | Adds to `footnotes.xml` + `w:footnoteReference` in a tracked run. |
| `set_hyperlink {anchor, find, url\|bookmark}` | `w:hyperlink` with a new external rel (URL validated: http(s)/mailto only). |
| `insert_field {anchor, find?, kind: PAGE\|NUMPAGES\|DATE\|REF\|TOC, args}` | Complex field; the cached result is computed where possible. |

**Lists**

| Op | Notes |
|---|---|
| `set_list {anchor\|range, list: "bullet"\|"numbered"\|"continue:<anchor>"\|"none", level?}` | Reuses an existing `w:num` that matches the target look (found by abstractNum signature) before minting a new one. Never edits a shared `abstractNum` in place (that would renumber unrelated lists). |
| `restart_numbering {anchor, start}` | New `w:num` with `lvlOverride/startOverride`. |
| `change_level {anchor, delta}` | `w:ilvl` via `pPrChange`. |

**Tables**

| Op | Notes |
|---|---|
| `insert_table {after, rows, cols, header_row?, style?: "inherit"\|<id>, data?}` | Inherits the document's most-used table style. Width = text width. Tracked as inserted paragraph marks + `w:trPr/w:ins` per row. |
| `insert_row {table, after_row, cells?}` / `delete_row` | `w:trPr/w:ins` / `w:trPr/w:del` (Word's tracked row changes); the new row clones the reference row's `trPr`/`tcPr`. |
| `set_cell {table.rR.cC, text}` | Delegates to `replace_text` / `insert_paragraph` inside the cell's story. |
| `insert_column` / `delete_column` | Word has no tracked column op: per-row cell insert/delete with `w:cellIns`/`w:cellDel`, plus `tblGridChange`. Marked **beta**; Word's support for tracked cell insertion is uneven, so the default in track mode is to warn and ask. |
| `merge_cells` / `split_cell` | Not trackable in Word. Allowed only in direct mode. |

**Images and drawings** — see §8.

**Comments**

| Op | Notes |
|---|---|
| `add_comment {anchor, find?, text, author?}` | `commentRangeStart/End` + `commentReference` run; writes `comments.xml` **and** `commentsExtended` (`w15:paraId`), `commentsIds` (`w16cid:durableId`), `commentsExtensible` (`w16cex:dateUtc`) and `people.xml`, so threading and resolve state work in modern Word. All paraIds `< 0x80000000` (larger values fail schema validation). `w:date` always set. |
| `reply_comment {comment, text}` | `w15:paraIdParent`. |
| `resolve_comment {comment, done}` | `w15:done`. |
| `delete_comment {comment}` | Removes the range, the reference and all four part entries. |

**Review**

| Op | Notes |
|---|---|
| `accept {change_set\|revision_ids\|author\|all}` / `reject {…}` | §7.4. |
| `compare {other_docx}` | Produces a change set that turns this version into `other` (§11.3). |

**Templates and content controls**

| Op | Notes |
|---|---|
| `fill {values: {name: value}}` | Fills `{placeholder}` tokens (today's `docx_fill` contract) **and** `w:sdt` content controls by `w:tag`/`w:alias`, including data-bound ones (`w:dataBinding` → `customXml` item, updated too). Tracked or direct. |
| `list_fields` | Placeholders + content controls with type (plain/rich text, date, dropdown, checkbox) and allowed values. |

**Document**

| Op | Notes |
|---|---|
| `set_properties {title, subject, keywords, …}` | `docProps/core.xml`. |
| `set_track_changes {on}` | `w:settings/w:trackRevisions`. |

**Read tools (no mutation):** `outline`, `read(range|anchors, mode)`, `find(text|regex, story?)`,
`get_block(anchor)` (full formatting detail), `list_changes(filter)`, `list_comments`, `list_images`,
`get_image(asset)` (returns the bytes so a vision model can look at it), `lint()`.

### 6.3 The hard cases we design for up front

- **Text split across runs**, including runs separated by `w:proofErr`, `w:bookmarkStart`, `w:commentRangeStart`
  or field chars. Matching runs over the paragraph's *text projection*, then maps character ranges back to
  (run, offset) pairs, splitting runs where needed. Non-text siblings stay where they are.
- **Matching across a field.** If `find` spans a field result, the op is refused ("that text is a field").
- **Editing inside an existing revision:**
  - Inserting inside your own `w:ins` extends it.
  - Inserting inside someone else's `w:ins` splits it.
  - Deleting your own insertion removes the text outright.
  - Deleting someone else's insertion nests `w:del` inside their `w:ins`.
  - Text already deleted is invisible to `find` in accepted mode.
  These are Word's own rules; each is a test case.
- **Paragraph with the section break.** Its `pPr/sectPr` defines the section, so delete and merge must move the
  `sectPr` to the surviving paragraph.
- **Numbering on insert.** A paragraph inserted after list item 3.1 continues the same `numId`/`ilvl`, so Word
  will show 3.2 and renumber the rest. We recompute labels and send the renumbered blocks in the patch.
- **Bookmarks and cross-references.** Deleting a paragraph that holds a bookmark referenced by a `REF` field
  triggers a lint warning ("article 3.1 is referenced from 5.2").
- **`xml:space="preserve"`** set whenever a `w:t` starts or ends with whitespace.
- **rsids** — never generated. Word adds them; they're optional.

## 7. Tracked changes (`track/`)

### 7.1 Authorship and identity

- Every revision gets an `author` (configurable: "AI assistant", or the agent and department name) and a
  `w:date`. Word historically writes local time with a "Z", so we also write the true UTC time in
  `w16du:dateUtc`, as current Word does (namespace declared and added to `mc:Ignorable`).
- Same-author edits collapse, as in Word: deleting text from your own open insertion removes it outright, so
  an AI that changes its mind leaves no noise. "Your own" means any pending `w:ins` with your `w:author`,
  also one from an earlier change set or session. A replace hunk wholly inside such an insertion rewrites it in
  place: the new text stays in that insertion and its change set (rejecting that change set removes it, and
  rejecting both restores the text from before either). A hunk that reaches outside it is tracked as usual, so
  rejecting its change set alone restores the text from before it.
- A revision element split by an edit (someone else's `w:ins` cut by an insertion, a run with a `w:rPrChange`)
  gets a fresh `w:id` on the new half; that id joins the original's change set.
- Revision `w:id`s come from one counter seeded by the max id across **all** annotation elements in **all**
  stories: `ins`, `del`, `moveFrom`, `moveTo`, the `*Change` elements, comments and bookmarks. Word tolerates
  duplicates across some types but not all; one shared space is always safe.
- **Change sets** (our concept) group revision ids. Word has no notion of them, so the mapping
  `change_set → [revision ids]` lives in a **sidecar** stored next to the document by the host (and optionally
  in a `customXml` part, so it travels with the file). If the sidecar is lost, revisions still show up in Word,
  just ungrouped.

### 7.2 Word-level diff for replaces

Tokenise the old and new strings into words, whitespace runs and punctuation, then run a Myers / patience diff.

- Equal tokens keep their original runs untouched (same `rPr`, same XML).
- Deleted tokens are wrapped in `w:del` *within their original runs* (so a deleted superscript stays
  superscript in the redline).
- Inserted tokens go into a new run in `w:ins`, with `rPr` copied from the left neighbour token's run (or the
  right one at the start of a paragraph).
- **Heuristic:** if more than 60% of the tokens change, emit one delete plus one insert for readability.

This directly fixes a failure seen in evaluation: a replace across mixed formatting lost a superscript
ordinal suffix.

### 7.3 What we write, per change type

| Change | Markup |
|---|---|
| Insert text | `<w:ins w:id w:author w:date><w:r>…<w:t>` |
| Delete text | `<w:del …><w:r>…<w:delText>` |
| Insert paragraph | Runs in `w:ins` **and** `w:pPr/w:rPr/w:ins` (paragraph mark) |
| Delete paragraph | Runs in `w:del` **and** `w:pPr/w:rPr/w:del` |
| Format run | `w:rPr/w:rPrChange` with the full old `rPr` |
| Format paragraph | `w:pPr/w:pPrChange` with the full old `pPr` (without `sectPr`/`rPr`) |
| Move | `w:moveFromRangeStart/End` + `w:moveFrom`, `w:moveToRangeStart/End` + `w:moveTo` |
| Table row | `w:trPr/w:ins` / `w:trPr/w:del` |
| Table props | `w:tblPrChange`, `w:trPrChange`, `w:tcPrChange`, `w:tblGridChange` |
| Section | `w:sectPrChange` |
| Image insert / delete | the `w:drawing` run inside `w:ins` / `w:del` |
| Numbering change | `w:pPrChange` holding the old `numPr` |

**Marks that can't merge.** Accepting a deleted mark (or rejecting an inserted one) merges the paragraph into the
next *paragraph*. When a table, a block content control or the end of the container (body, cell, header, …)
follows instead, nothing merges: Word and Docxodus keep the paragraph, so a tracked mark there would leave an
empty paragraph behind. The rule, for `delete_block`, `insert_paragraph`, `move_block` and page/column breaks:

- A mark that can't merge is not tracked itself. Its revision goes on the nearest preceding paragraph's mark,
  walking back over paragraphs whose mark already carries that kind (`w:del` for a delete, `w:ins` for an
  insert), so a run of deleted paragraphs before a table resolves to one merge chain ending at the last one.
- The walk stops at a table, a block content control, a paragraph carrying a `sectPr`, or the start of the
  container. With no stand-in, a delete removes the runs only and leaves an empty paragraph in every view,
  and an insert tracks the new paragraph's own mark, so rejecting it leaves an empty paragraph, as Word does.
- A mark can carry both `w:ins` and `w:del` (inserted as a stand-in, then its paragraph deleted); accept and
  reject each drop it.
- Open: on a merge we keep the first paragraph's `pPr`; Word may give the merged paragraph the surviving
  mark's properties instead. The text is identical either way; paragraph formatting can differ (unverified).

### 7.4 Accept / reject

These are implemented structurally, per revision type. Accepting an insert unwraps it; rejecting it removes
the content. Deletes are the mirror image. Accepting a format change drops `*Change`; rejecting it restores
the snapshot. A paragraph-mark delete merges the paragraph with the next one; before a table or a block content control the paragraph stays (§7.3). A move resolves both ends.

Scopes: one revision, one change set, by author, or all.

**Invariants we test on every op sequence:**

- reject-all(edited) is text-identical to reject-all(original) in every story;
- accept-all(edited) is text-identical to the intended result.

The evaluation scorer does exactly this.

## 8. Images and drawings (first-class)

### 8.1 Reading

For every `w:drawing`, in every story, we extract:

| Field | Source |
|---|---|
| Placement | `wp:inline` vs `wp:anchor` (+ `wrapSquare`/`wrapTight`/`wrapTopAndBottom`/`wrapNone` + `behindDoc`, `positionH/V`) |
| Size | `wp:extent cx/cy` (EMU; 914 400 per inch, 360 000 per cm) |
| Picture | `a:blip r:embed` → rel → media part; `r:link` = external (never fetched, see §12) |
| Crop / transform | `a:srcRect l/t/r/b` (1/1000 %), `a:xfrm rot` (60 000ths of a degree), `flipH/flipV` |
| Accessibility | `wp:docPr descr`, `title`, `a16:decorative` |
| SVG | `asvg:svgBlip` extension (an SVG part + PNG fallback in the main blip) |
| Charts | `c:chart` → `word/charts/chartN.xml` (+ embedded workbook) |
| SmartArt | `dgm:relIds` → data/layout/colours/quickStyle parts + `dsp` drawing fallback |
| Shapes / text boxes / groups | `wps:wsp`, `wpg:wgp`, `wps:txbx/w:txbxContent` (text is a story, §4.2) |
| Legacy | VML `w:pict` / `v:shape` / `v:imagedata r:id`, also inside `mc:Fallback` |
| Watermark | VML or DrawingML shape in a header, usually `behindDoc` |
| OLE | `w:object` (`o:OLEObject`), shown as its preview image + an "embedded object" badge |

### 8.2 Serving images to the viewer

- **Assets are content-addressed:** `asset id = sha256(bytes)[:16]`. The service serves
  `/assets/{id}` with immutable caching. The same logo in 12 headers is one asset.
- **Browser-safe formats** (PNG, JPEG, GIF, WebP) are served as-is.
- **Converted formats:**
  - EMF/WMF (common in older Word docs and logos) and TIFF go to PNG through a converter hook. The default
    converter is LibreOffice headless, run in the same no-network container as PDF export; results are cached
    by asset id.
  - **SVG** is served only after sanitising (no script, no external refs, no `foreignObject`), else the PNG
    fallback is used.
- **Charts:** v1 shows a placeholder card with title and type, plus a **rendered PNG** from LibreOffice
  (cached). v2 draws the common types (bar/line/pie) from the chart part's cached values (`c:numCache`,
  `c:strCache`) with a chart library, so the AI's data edits show live.
- **SmartArt** renders from its `dsp` drawing fallback when present (it is a ready shape tree), else a PNG from
  LibreOffice.
- **Floating images** are approximated: `wrapSquare` left or right becomes a CSS float; `behindDoc` and
  `wrapNone` become positioned blocks with an "anchored" badge. Exact placement is what the PDF preview is for.
- **No layout jump:** the viewer reserves the box (`extent`) before the image loads.
- **Click selection:** clicking an image selects its anchor (`img4`) for the chat ("make the logo smaller",
  "replace this photo").

### 8.3 Writing

| Op | Behaviour |
|---|---|
| `insert_image {after\|anchor+find (inline), asset, width?: cm\|"text-width"\|"original", alt, decorative?, wrap?: "inline"}` | Dedupes the media part by hash; adds a rel **from the story's own part** (headers have their own `.rels`); a content-type Default for the extension; a `wp:inline` with a `wp:docPr id` unique across *all* stories; `pic:pic` with `a:blip`. Size defaults to the image's natural size at its DPI, clamped to the section's text width (page width − margins − gutter). **Alt text is required** unless `decorative`. Tracked as a `w:drawing` run inside `w:ins`. |
| `replace_image {image, asset, fit: "keep-box"\|"keep-width"\|"natural"}` | Swaps the `r:embed` target (new part; the old one is garbage-collected if unreferenced). `keep-box` letterboxes by cropping (`srcRect`) so layout doesn't move. Tracked as a delete of the old drawing run plus an insert of the new one. Untracked replace is allowed in direct mode. |
| `resize_image {image, width\|height\|scale}` | Keeps aspect; updates `wp:extent` and `a:ext` together (they must agree; Word lays out from `wp:extent`); grows `wp:effectExtent` when rotated. Word has no revision type for a resize or crop, so it is untracked and recorded only in our sidecar change set (undo works there), or tracked as delete + insert when the user asks. |
| `set_alt_text {image, alt, title?, decorative?}` | `wp:docPr`; untracked (Word doesn't track it either). |
| `delete_image {image}` | Tracked: drawing run in `w:del`. |
| `anchor_image {image, wrap, align}` | **P2.** Converts inline ↔ anchored. Fiddly, rarely needed by AI edits. |

Supported image inputs, in priority order:

1. User upload, through the host (validated, re-encoded, EXIF GPS stripped).
2. An asset already in the document, or in another document of the same tenant.
3. A generated chart: the host renders a chart spec to PNG and keeps the spec in the sidecar, so "change the
   Q3 figure" can re-render it.
4. A generated image from an image model, through the host's hook.
5. A URL, fetched **by the host** with SSRF protection; the engine never fetches.

Image decoding (dimensions, re-encode) happens in the Rust `image` crate with explicit limits (maximum pixels
and allocation per image, the decompression-bomb guard). Reading dimensions only parses the header.

## 9. Export

| Output | How |
|---|---|
| **DOCX — with changes** (default) | The current package as stored. `w:trackRevisions` is left as the user set it. |
| **DOCX — clean** | A copy with accept-all applied (optionally: only the AI's change sets; other people's revisions remain). Comments optionally stripped. |
| **DOCX — original** | The uploaded bytes, untouched. |
| **PDF — clean / with markup** | LibreOffice headless via **Gotenberg** (a self-hosted container with no network egress), from the DOCX above. Fields (page numbers, TOC) are updated during conversion. PDF/A-2b optional for archiving. |
| **Markdown / plain text** | The projection, for search indexing and for a host's own memory or search (§11). |

### 9.1 Fonts

PDF fidelity depends on fonts:

- **Bundled:** the metric-compatible families (Carlito, Caladea, Liberation, Arimo/Tinos/Cousine), plus Noto
  for wide script coverage.
- **Embedded in the document:** `word/fonts/*.odttf` with `w:embedRegular`/…. LibreOffice reads these on
  import.
- **Per tenant:** an optional "house fonts" upload (the client's corporate font, which they license) mounted
  into the converter.
- **Missing fonts are reported:** the export response lists fonts that were substituted, so "why does the PDF
  look different" has an answer.

### 9.2 Field and TOC refresh

When the AI adds a heading, the TOC's cached text is stale.

- **Export:** the PDF path updates fields. The DOCX keeps the old cache, with `w:dirty="true"` on that one TOC
  field, so Word refreshes it on open. That is narrower than `w:updateFields`, which triggers a dialog.
- **Viewer:** the viewer recomputes the TOC itself from the outline, so the screen is right immediately.

### 9.3 Print preview

A **"Print preview"** tab renders the current version to PDF (debounced, cached per version) and shows it with
pdf.js. That gives an exact page view on demand without us building a layout engine.

## 10. Validation and quality (`validate/`, `lint/`)

| Layer | When | What |
|---|---|---|
| **Invariants** | Every transaction, before commit | Each item below; a failure rolls the whole transaction back. |
| **XSD** | Tests, and optionally at runtime | ECMA-376 Transitional schemas + the [MS-DOCX] extension schemas, with lxml in the Python test harness, **after** markup-compatibility processing (otherwise the w14+ markup fails). Ecma's licence for redistributing the XSD files is unclear, so the test job downloads them instead of the repo bundling them. |
| **Oracles** | CI only | Open XML SDK validator (container, or `@xarsh/ooxml-validator` via npx: no .NET needed). It has false positives on newer w16 markup and misses things like `docPr` collisions, which our invariants catch. **Clippit's WmlComparer** (MIT, maintained OpenXmlPowerTools fork) diffs input against output as an independent check that the only change is the intended one. LibreOffice open + re-save + render. |
| **Word oracle** | Before each release | Real Word decides. Either a Windows VM driving Word through COM and detecting the repair prompt, or Microsoft Graph `…/content?format=pdf` (Word for the web's converter, needs an M365 tenant): a file Word can't open fails to convert. Word on the Mac, scripted, is the cheap local version. |
| **Lint** (document semantics) | After a change set, shown to the agent | Each item below. These are mistakes that are valid XML but wrong documents, which matters most for legal and HR text. |

**Invariants checked on every transaction:**

- every rel target exists;
- every part has a content type;
- ids are unique: `docPr id` (and its `pic:cNvPr id`) across all stories, revision ids, comment ids,
  paraIds, bookmark ids (paired start and end);
- every `w14:paraId`, `w14:textId`, `wp14:anchorId` and `wp14:editId` is `> 0` and `< 0x80000000`;
- children are in **schema order** inside every `pPr`, `rPr`, `sectPr`, `tblPr`, `settings` and `wp:anchor`
  (Word "repairs" order violations), so we insert children through an order table, never with `append`;
- every table cell ends with a `w:p`, even after a nested table, and each row's `gridSpan`s add up to the
  grid;
- no namespace declaration that `mc:Ignorable` names has been dropped;
- `abstractNum`s come before `num`s in `numbering.xml`;
- `w:ins`/`w:del` are nested legally;
- every comment has a range and a reference;
- `sectPr` sits last in the body;
- `extent` and `a:ext` agree;
- there are no empty `w:t` with leading or trailing spaces lacking `xml:space`;
- the XML is well-formed.

**Lint rules:**

- broken cross-references (`REF` to a deleted bookmark);
- hard-coded article numbers ("zie artikel 3.2") that no longer match the numbering;
- numbering gaps;
- leftover `{placeholders}`;
- defined terms used inconsistently ("werkgever" vs "Werkgever");
- doubled spaces and punctuation;
- an image without alt text;
- headings out of order.

**The scorecard is the evaluation harness, kept as a regression suite.** The engine must beat Docxodus on all six
dimensions before it ships: untouched paragraphs identical, replace formatting, insert pPr, insert numbering,
delete, comments; plus accept and reject views.

## 11. Service and host integration

### 11.1 Reference service (`quillwork-server`, part of the open-source package)

```
POST   /documents                      upload → {id, version}
GET    /documents/{id}/view            view model (JSON)              ?version=
GET    /documents/{id}/projection      LLM text                       ?mode=&range=
POST   /documents/{id}/ops             {base_version, ops[], label?, author?} → {version, change_set, patch, lint}
GET    /documents/{id}/events          SSE: patch | change_set | comment | lint | export_ready
POST   /documents/{id}/changes/{cs}/accept | /reject
GET    /documents/{id}/versions        list; each version = immutable docx snapshot ref + change set
GET    /documents/{id}/export          ?format=docx|pdf&variant=redline|clean|original
GET    /documents/{id}/assets/{asset}
POST   /documents/{id}/images          upload an image asset (validated)
```

- **Storage behind an interface:** `DocumentStore` (snapshot bytes, sidecar, versions) and `AssetStore`. The
  reference implementation is a filesystem or S3.
- **Versions:**
  - Each committed change set is a version, stored as an op log plus a **full snapshot every N versions** and
    at every accept/reject. Any version can be rebuilt; the latest is always a snapshot.
  - Undo = reject the change set (tracked mode) or restore the previous snapshot (direct mode).
- **Concurrency:** one writer per document, a lock around apply. The user and the agent both submit ops; guards
  make stale ops fail cleanly instead of corrupting the document.

### 11.2 How a host embeds it

A host application embeds `DocumentService` in process and implements three
interfaces: a `Store` for version bytes and records (for example database rows plus S3 blobs), a
`LockProvider` (for example a Postgres advisory lock) and a `Notifier` (for example `LISTEN`/`NOTIFY`). Its
agent loop exposes the document tools; its UI shows the viewer next to its chat. Tracked changes are the review
step for in-document edits; actions that leave the document (sending it, publishing it) stay behind the host's
own approval flow. The host keeps the projection as plain text for its search and memory features, so those
keep working without knowing about OOXML. `docs/SERVER.md` describes the interfaces.

### 11.3 Compare and re-upload

`compare(base, other)` aligns paragraphs: first by paraId, then by an LCS of normalised paragraph text with a
similarity threshold for "modified". Changed paragraphs then go through the word-level diff (§7.2).
Formatting-only changes become `rPrChange`/`pPrChange`; moved blocks above a size threshold become tracked
moves. Tables are aligned row by row. Images are compared by asset hash.

The output is a change set authored as the uploader. This is also a standalone feature ("what did the client
change in the version they sent back?") and a strong open-source selling point. OpenXmlPowerTools'
`WmlComparer` is the reference to learn from.

## 12. Security

| Threat | Defence |
|---|---|
| Zip bombs | Caps: entries ≤ 10 000, total uncompressed ≤ 512 MB (configurable), per-entry ratio ≤ 200:1, streaming reads with a byte counter. |
| Zip path tricks | Reject absolute paths, `..`, backslashes, duplicate names (case-insensitive), and names that don't match the OPC part-name grammar. |
| XXE / entity expansion | quick-xml never loads DTDs, resolves external entities or touches the network. Any `<!DOCTYPE` is rejected outright (OOXML never needs one), so no custom entity can be declared either. Element depth and text-node size are capped. |
| External relationships | `TargetMode="External"` rels are listed in metadata and **never fetched**. This covers remote images (tracking pixels, NTLM leaks), `attachedTemplate` (template injection), `oleObject` links, `subDocument`, `frame`. The viewer shows remote images as a placeholder. Export option: "strip external links". |
| Active content | Macros, OLE (Packager objects, linked `oleObject`s: the Follina CVE-2022-30190 pattern), ActiveX and `DDE`/`DDEAUTO`/`INCLUDETEXT`/`INCLUDEPICTURE`/`LINK` fields (including ones hidden in `w:delInstrText`) are preserved but never executed or evaluated. They are flagged in metadata so the host can warn or block. A `vbaProject.bin` inside a `.docx` (wrong content type) is flagged as suspicious. "Preserve unknown markup" never means "pass active content through silently". |
| Prompt injection via the document | Document text reaches the model only inside a fenced, labelled data block. Hidden, tiny, white or zero-width text is **marked** in the projection (§5.2). Comments from third parties are labelled as such. The host's system prompt says document content is data, not instructions. |
| Images | Decoding in the `image` crate with pixel and allocation limits; SVG sanitised; EMF/WMF converted only inside the sandboxed converter. |
| Converters | LibreOffice/Gotenberg runs in its own container: no network, read-only rootfs, CPU/memory/time caps, one document per call. |
| Resource use | Per-call timeouts; op batch size caps; projection token budgets. |

## 13. PowerPoint next; Excel later and not promised (same core)

The OPC layer, assets, security, validation framework, the service, change sets and the viewer shell are
shared. What differs:

| | PowerPoint (`.pptx`) | Excel (`.xlsx`) |
|---|---|---|
| Model | presentation → slides → shape tree (`p:sp`, `p:pic`, `p:graphicFrame`, `p:grpSp`); text bodies `a:txBody/a:p/a:r`; layouts and masters; placeholders by `type`/`idx` | workbook → sheets → rows/cells; shared strings; styles (`cellXfs`); formulas; defined names; tables; charts |
| Projection | Slide by slide: title, placeholders and shapes with anchors (`s3.sh5`), speaker notes, images with alt text | Per sheet: used range as a table with cell refs, formulas shown as `=…` next to cached values, named ranges, tables |
| Ops | Replace text in a shape; add, delete, duplicate or reorder slides (from a layout); set notes; insert or replace image; table cells; chart data | Set a cell value or formula; insert or delete rows/cols (refs shifted); format; add sheet; table ops; chart data |
| Review | **No native tracked changes.** Change sets live only in the sidecar. The UI shows before/after per slide (our renderer) and offers accept/reject; reject restores the pre-change XML from the snapshot. Comments: modern pptx comments (`ppt/comments/modernComment_*.xml`). | **No usable native tracking** (shared-workbook revisions are legacy). Same sidecar change sets, with a cell-level diff view (old → new, highlighted). Threaded comments + notes. |
| Viewer | Our own slide renderer: absolutely positioned shapes, text, images. Thumbnails from LibreOffice for exactness. | A grid renderer with formatting and frozen panes; formulas recalculated by LibreOffice on export (or a formula engine later). |
| Gotchas | Slide ids in `p:sldIdLst` start at 256; adding a slide touches the id list, rels, content types and `p14:sectionLst`. Placeholders inherit through layout and master. `a:normAutofit fontScale` means the renderer must shrink overflowing text. | Delete `calcChain.xml` whenever formulas change, or Excel repairs the file; set `calcPr fullCalcOnLoad="1"`. Inserting rows shifts every reference: formulas, merges, conditional formats, validations, tables, defined names, charts. Threaded comments need the legacy comment plus its `vmlDrawing` as a fallback. |
| Libraries | python-pptx (MIT) is the only real editing library and is in maintenance; we use our own OPC core plus its ideas. | openpyxl **drops** charts and images it loads, so it can't be our model either; the same raw-XML approach as Word. |
| Effort | Medium. The slide renderer is the big part. | Medium-high. Formula reference shifting is the hard part. |

## 14. Testing

- **Corpus.** Committed:
  - our own generated fixtures, one per feature in this doc;
  - public files with compatible licences: Apache POI test documents (Apache-2.0) and selected LibreOffice
    `sw/qa` documents (MPL-2.0, kept in a separate `corpus/third_party/` with notices).
  **Never committed:** client documents. Real-world documents used for evaluation run only from a
  local path outside the repo (`QUILLWORK_PRIVATE_CORPUS`).
- **Round-trip property test:** for every corpus file, `save(load(f))` gives back the whole file byte-identical, and a save after an edit leaves every clean zip entry byte-identical.
- **Op property tests (proptest in Rust; Hypothesis through the Python binding):** random op sequences on
  random corpus files; after each: invariants pass,
  XSD passes, reject-all text equals the original text, accept-all text equals the model's expected text.
- **Golden files:** the projection and view model per corpus file; diffs are reviewed in PRs.
- **Oracle CI job (nightly):** the Open XML SDK validator and a LibreOffice render of every output; render
  diffs are image-compared against the last accepted baseline.
- **Word check (manual, before release):** a scripted run through real Word, flagging "repair" prompts.
- **Fuzzing:** `cargo-fuzz` (libFuzzer) on the zip splicer, the token tree and op application, run in CI
  and for 24 h before each release (target in §1).
- **Token metrics as golden numbers:** tokens per page, anchor overhead and first-call size per corpus file,
  counted with Anthropic's token counter. A change that pushes any of them past the §1 targets fails CI.
- **Budgets checked in CI (§1):** load and project a 200-page contract in < 500 ms; one op in < 30 ms; a patch
  < 10 KB typical; wheel, WASM and viewer bundle sizes; RSS per open document.

## 15. Open-source packaging

| Item | Decision |
|---|---|
| Licence | **Apache-2.0**: permissive like MIT, plus an explicit patent grant, which matters near the Office formats. Not copyleft, so companies can adopt it. |
| Layout | One monorepo: `spec/`; `crates/quillwork-core` (engine), `crates/quillwork-py` (PyO3 → the `quillwork` wheel), `crates/quillwork-wasm` (`@quillwork/core-wasm`); `python/quillwork-mcp`, `python/quillwork-server`; `js/viewer` (`@quillwork/viewer` + `@quillwork/react`); `corpus/`, `docs/`, `bench/`. Wheels are built for Linux (x86-64, arm64), macOS and Windows by maturin in CI. |
| Boundary | Nothing host-specific inside: no auth, tenants, memory paths, billing or prompts. Each host has a thin adapter of its own. |
| Where it lives | Its own repo from day one, consumed by hosts as a pinned dependency. |
| Naming | No "Word", "Office" or "docx" in the product name (Microsoft trademarks). "Quillwork" is a working name. |
| AI guidance | Ship a `SKILL.md`/prompt pack with the MCP server: how to read the outline first, make minimal edits, never hard-code article numbers, write alt text. The prompt is part of what makes the edits good. |
| Docs | A feature-support matrix (P0/P1/P2 per OOXML feature) generated from the test corpus, so the claims stay honest. |

## 16. Feature tiers

- **P0, must edit:**
  - paragraphs, runs, styles, lists and numbering, tables (rows/cells), images (inline), comments, tracked
    changes (all text types, row ins/del, format changes), headers/footers text, footnotes, hyperlinks,
    simple fields, `{placeholders}` and content controls, core properties.
- **P1, render + preserve, limited edit:**
  - floating images, text boxes, charts (image), SmartArt (image), section/page setup, TOC (refresh on
    export), bookmarks/cross-references (lint), tracked moves, column ops, math (`m:oMath`, rendered via
    MathML), bidi/RTL text.
- **P2, preserve only:**
  - OLE objects, ActiveX, macros, glossary/building blocks, customXml (except data-bound controls),
    VML-only shapes, embedded fonts, digital signatures, mail-merge setup, frames, legacy form fields.
- **Signatures:** any edit invalidates an XML digital signature. Signed documents open **read-only**, with a
  notice.

## 17. Build plan

| Phase | Scope | Exit criterion | Size |
|---|---|---|---|
| **0. Spike + spec + harness** | The 2-week Rust spike (§2); op and view-model JSON Schemas, anchor and projection grammar, corpus seed, evaluation harness turned into the regression suite | **Go/no-go on Rust** against the §2 criteria; schemas reviewed; harness runs against Docxodus **and Adeu** as baselines | ~3 wk |
| **1. Read core** | OPC + hardened load/save, stories, style and numbering resolution, fields (read), drawings (read), anchors, both projections, asset extraction | No-op round trip byte-identical on the whole corpus; golden projections | ~2–3 wk |
| **2. Edit + track** | Text, paragraph, list and comment ops; word-level diff; full tracked-change writer; accept/reject; invariants; XSD + Open XML SDK in CI | **Beats Docxodus on every scorecard row** on the private evaluation set | ~3–4 wk |
| **3. Viewer + service + first host** | Viewer package, SSE patches, change-set cards, the document screen with chat, doc tools in the agent loop, `documents` tables, blob storage | A real-world document edited end to end by chat, accepted, exported to Word, opens clean in Word | ~3 wk |
| **4. Images + tables + PDF** | Image insert/replace/resize/alt, EMF conversion, table row/cell ops, Gotenberg + fonts, Print preview | Logo replace + table edit + PDF export on the corpus | ~2–3 wk |
| **5. Breadth** | Headers/footers/footnotes edits, fields and TOC, content controls, moves, compare + re-upload, lint rules, inline user edits | Re-upload from Word shows the user's edits as a change set | ~3–4 wk |
| **6. Open-source v0.1** | Docs, feature matrix, MCP server + skill, licence and corpus notices, public repo | — | ~1 wk |
| **7. pptx** | §13. xlsx is not promised (§1) | — | ~4–6 wk |

Total for DOCX to production in the first host: **about 3.5–4.5 months** of focused work (phases 0–5). The spike
adds two weeks up front; the token tree's 2–4 weeks sit inside phase 1 if the spike says go.
Docxodus remains available as the fallback and as the yardstick until phase 2 passes.

## 18. Decisions to make

1. **Name.** Keep "Quillwork" or pick another; check the GitHub org and the domain.
2. **Licence.** Apache-2.0 (recommended) or MIT.
3. **Repo.** A separate repo from day one (recommended), or start inside the first host and extract later.
4. **Author label.** On tracked changes: "AI assistant", the department ("HR-agent"), or the user's name +
   "(AI)".
5. **v1 inline editing.** Should users type directly in the viewer (tracked as them), or chat plus "Open in
   Word" only? Recommended: plain text edits per paragraph in v1, nothing richer.
6. **Adeu first?** Adeu (MIT, Python, MCP) is the closest open project to our AI-facing layer. Run it through
   the evaluation harness in phase 0: if it is strong, we could contribute to it or reuse parts instead of writing §5
   from scratch. Recommended: benchmark it, but keep our own core either way, because Adeu has no viewer,
   view model or service.
7. **Rust or Python core.** Decided by the 2-week spike against the criteria in §2, not by preference. No-go
   means the Python + lxml core with the same splicer, anchors and diff design.

## 19. Prior art (survey, 2026-10-26)

Nothing MIT- or Apache-licensed covers the whole stack. Each part has a good reference to learn from, and a few
are reusable:

| Project | Licence | What it is | Our use |
|---|---|---|---|
| **Docxodus** | MIT (.NET) | Anchors, tracked edits, validator | The baseline to beat; the fallback until phase 2 |
| **Adeu** | MIT (Py/Node) | "Virtual DOM for Word": CriticMarkup projection, semantic appendix, ambiguity gate, native tracked changes, MCP | Benchmark; learn from its projection (§5.2) |
| **Clippit** / OpenXmlPowerTools | MIT (.NET) | `WmlComparer` (real `w:ins`/`w:del` compare), `RevisionAccepter` | Test oracle (§10); the reference for `compare` (§11.3) |
| **docx-preview** (docxjs) | Apache-2.0 (TS) | docx → semantic HTML, with parse and render split | Reference for the viewer's rendering of styles, tables and lists |
| **Eigenpal docx-editor** | Apache-2.0 (1.x on npm; 0.x was MIT). No separate proprietary package found | Round-trip model + DOM-free layout engine | Read for ideas; usable as a reference |
| **SuperDoc** | **AGPL-3.0** / commercial | The closest complete product: OOXML-backed model, editor, headless API, tracked writes, MCP | Study the API design only; no code |
| python-docx / python-pptx | MIT | Proxy objects over lxml | Same pattern as §4.1; not used as a dependency (no redlines) |
| mammoth | BSD-2 | docx → clean HTML or Markdown | Not needed: our projection replaces it |
| LibreOffice / Gotenberg / Collabora | MPL-2.0 | Rendering, conversion | External PDF and conversion service only |
| docx4j | Apache-2.0 (Java) | Complete schema classes, `diffx` | Reference for schema edge cases |

**How the big players do it:**

- **Harvey** and **ChatGPT for Word** run *inside* Word as add-ins (Office.js), so Word is the renderer.
- **Anthropic's docx skill** and OpenAI's former `doc` skill edit the XML directly and check the result
  visually with LibreOffice PDF → PNG.
- None of them ships its own pixel-accurate renderer. That supports the split chosen here: our own reading
  view, plus an exact PDF from LibreOffice.

**Do not depend on or copy from:**

- SuperDoc, ONLYOFFICE and KitchenSink4Word (AGPL);
- pandoc (GPL; calling it as a subprocess is fine);
- docxtpl (LGPL);
- Anthropic's skill scripts (proprietary).

docxtemplater is fine only under its MIT option. For xlsx later: SheetJS's npm package is frozen at an old,
flagged version, and ExcelJS is stalled. Another reason the shared raw-XML core is the plan.
