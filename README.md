# Quillwork

Quillwork lets an AI agent edit Word documents without breaking them.

![An AI agent changes one clause in a Word contract and adds another; the letterhead, numbering, table, styles and footer stay untouched](docs/quillwork-demo.gif)

The agent reads a compact text view of the document, where every paragraph, table row and
comment has a short anchor. It sends small JSON edits against those anchors. Quillwork writes
each edit into the original `.docx` as a real Word tracked change. Every part of the file the
edit did not need to touch stays byte for byte as it was, so letterheads, numbering, styles,
fields, images and existing tracked changes all survive. A person then opens the file in Word
and accepts or rejects the agent's changes like any colleague's.

Status: **alpha (0.0.x)**. The API, the op language and the sidecar format can still change
between releases. See [Limitations](#limitations).

## Why

Most ways of letting an LLM change a Word file rebuild the file:

- converting to Markdown or HTML and back,
- loading it into an object model and re-serialising it,
- asking the model to rewrite the XML.

Each of these loses things the model never meant to change, such as a custom letterhead, a
numbering definition, a field code, a content control or someone else's tracked changes. The
result also arrives as a new document instead of a redline, so nobody can see what the AI
actually changed.

Quillwork never rebuilds the document. It parses the package into a tree that keeps the exact
source bytes of every element, and it splices edits into that tree. When it saves, untouched zip
entries are copied raw and untouched XML is written back verbatim. Every edit is a `w:ins`,
`w:del` or comment, attributed to the agent, so Word's own Review pane is the approval step.

## What the agent sees

```text
[b100 H1] Services Agreement
[b102 H2] 1 Scope and fees
[b103 #1.] The Consultant provides advice on employment policy for 16 hours per week.
[b104 #2.] The hourly rate is EUR 95, excluding VAT.
[t1 TABLE 3×3]
  [t1.r2] Senior adviser | 12 | EUR 95
```

After `{"op": "replace_text", "anchor": "b103", "find": "16 hours", "replace": "20 hours"}`,
the same line reads back as a redline:

```text
[b103 #1.] The Consultant provides advice on employment policy for {--16--}{++20++}⟨r1,2 AI⟩ hours per week.
```

The projection uses [CriticMarkup](https://criticmarkup.com/) for changes and comments. It is
paged to a token budget, and anchors stay stable across edits and sessions. The full grammar is
in [`spec/projection.md`](spec/projection.md).

## Install

Python 3.12 or newer:

```sh
pip install quillwork                    # the engine (Python binding)
pip install 'quillwork-server[mcp]'      # optional: MCP server and agent tools
```

Prebuilt wheels cover macOS arm64 and Linux x86_64 (manylinux 2.28). On other platforms pip
builds from source, which needs a Rust toolchain (1.88 or newer). The same wheels are attached
to each [GitHub release](https://github.com/gardan4/quillwork/releases).

## Quickstart

### Python

```python
from pathlib import Path

import quillwork

doc = quillwork.Document.open(Path("contract.docx").read_bytes())
doc.set_agent("AI")                  # the author name on tracked changes
print(doc.read())                    # the projection above: anchors, text, structure

result = doc.apply({
    "base_version": doc.version,
    "ops": [{"op": "replace_text", "anchor": "b103", "find": "16 hours", "replace": "20 hours"}],
})
print(result["changed"])             # the edited lines, as the agent would see them
Path("contract.edited.docx").write_bytes(doc.save())
```

To try it without a document of your own, `python examples/make_sample.py contract.docx` writes
a synthetic one that matches the anchors above, and
[`examples/edit_contract.py`](examples/edit_contract.py) runs a longer walkthrough that checks
what changed. A transaction is atomic: either every op applies or none does. `doc.help()` lists
every op with its parameters.

### MCP server (Claude Desktop, Claude Code and other MCP clients)

```sh
quillwork-mcp                       # stdio
```

```json
{ "mcpServers": { "quillwork": { "command": "quillwork-mcp", "args": ["--store", "/path/to/docs"] } } }
```

This exposes eight agent tools (`read_document`, `find`, `edit`, `format`, `table`, `comment`,
`review`, `advanced_op`) plus open, list and export. Their schemas are in
[`spec/tools/`](spec/tools/), and `spec/tools/export.py` emits them in Anthropic, OpenAI and
MCP shapes for your own agent loop. There is also an HTTP API (`quillwork-server`, extra
`[http]`). See [`docs/SERVER.md`](docs/SERVER.md).

### Rust

The core is a plain Rust library. It is not on crates.io yet, so depend on it from git:

```toml
quillwork-core = { git = "https://github.com/gardan4/quillwork" }
```

```sh
cargo run -p quillwork-core --example quickstart -- contract.docx contract.edited.docx
```

[`crates/quillwork-core/examples/quickstart.rs`](crates/quillwork-core/examples/quickstart.rs)
shows the same read, edit and save through `Engine`, with JSON in and JSON out.

## What works today

- **Lossless round trip.** A no-op open and save returns the input bytes exactly. After an
  edit, only the parts the edit needs change.
- **Text edits as tracked changes:** replace (word-level diffs, formatting kept),
  find-and-replace with an expected count, insert, delete, move, split and merge paragraphs.
- **Formatting:** run marks (bold, italic, underline and so on), paragraph style, alignment,
  indentation, spacing, page and section breaks.
- **Lists:** set a list, restart numbering, change level. Clause numbers are shown in the
  projection (`#1.`) but stay Word's to compute.
- **Tables:** insert a table, insert or delete rows and columns, set cells, merge and split
  cells.
- **Comments:** add, reply, edit, resolve, delete, with modern Word comment parts.
- **Review:** list, accept and reject changes by change set, author or id. Turn tracking on or
  off.
- **Reads:** outline, paged read, find (text or regex), single blocks, changes, comments,
  images, fields, and a lint pass.
- **Safety rails:** optimistic versioning (`base_version`), optional `expect` guards on every
  op, a structural check that rolls back any transaction that would make the package less
  valid than it was, and hidden or near-invisible text flagged in the projection, so a model is
  not steered by text a reader cannot see.
- **Viewer:** [`js/viewer`](js/viewer) is a framework-free TypeScript viewer for the engine's
  view model, with live patches and a React wrapper.

The op catalogue and its JSON Schemas are in [`spec/`](spec/). That directory is the contract
every binding implements.

## Measurements

These numbers come from [`bench/`](bench/). It runs on the synthetic documents and feature
fixtures in this repository, so you can reproduce it. The method and caveats are in
[`bench/README.md`](bench/README.md).

| Over 19 public test documents | Quillwork | python-docx |
|---|---|---|
| No-op open and save is byte-identical | 19 of 19 | 0 of 19 |
| One-word edit: other zip entries changed | 0 on every file | 6 to 12 per fixture |
| One-word edit: paragraphs changed | exactly 1 on every file | 2 or 3 on 4 of the files |

python-docx is a document-building library, not an editing engine. It is in the table because
it is what most people reach for first. On the feature fixtures, the projection an agent reads
is 2.9 to 13 times fewer tokens than the raw `document.xml`. Rendering fidelity in Word and
LibreOffice is checked separately by [`tools/oracle`](tools/oracle) and is not part of these
numbers.

## Limitations

- **Format:** `.docx` and `.docm` only. Encrypted or password-protected files are refused.
  There is no `.doc`, `.odt`, `.xlsx` or `.pptx` support.
- **Not implemented yet:** footnote and endnote insertion, hyperlinks, inserting fields, all
  image edits (images can be read and listed), document compare, template fill, and document
  properties. `doc.help()` marks each of these as `(not implemented)`, and the engine rejects
  them.
- **Known issue:** inserting a row after a vertically merged row can drop text from the merged
  cell.
- **Bindings:** Python and Rust only. The WASM binding (`crates/quillwork-wasm`) is a
  placeholder. The viewer is written but not yet published to npm.
- **Anchors:** anchors survive across sessions only when you keep the sidecar from
  `save_with_sidecar()` and pass it back to `open()`. Without it, anchors are re-minted from
  the document.
- **Stability:** this is 0.0.x. Expect breaking changes to the op language and the sidecar
  format, which will be recorded in [CHANGELOG.md](CHANGELOG.md).

## Repository layout

| Path | What |
|---|---|
| `crates/quillwork-core` | The Rust engine: zip splicer, raw-span XML tree, Word model, anchors, projection, ops, validator |
| `crates/quillwork-py` | The Python binding (`quillwork`, PyO3, abi3 wheels) |
| `python/quillwork_server` | `quillwork-server`: document service, the eight agent tools, MCP server, HTTP API |
| `js/viewer` | `@quillwork/viewer`, the TypeScript viewer |
| `spec/` | The language-neutral contract: op and view-model schemas, projection grammar, error codes, tool definitions |
| `corpus/` | Synthetic test documents and the scripts that generate them |
| `bench/` | The reproducible fidelity and token benchmark |
| `tools/oracle` | Rendering and validation checks against LibreOffice and an Open XML validator |
| `docs/` | Design record (`DESIGN.md`), binding and server notes |

## Contributing

Bug reports with a document that reproduces the problem are the most useful contribution. Use
a synthetic document, never a confidential one. See [CONTRIBUTING.md](CONTRIBUTING.md) for how
to build and test, and [SECURITY.md](SECURITY.md) for how to report a vulnerability privately.

## Where it comes from

Quillwork was built for [Foundworks](https://foundworks.ai), where AI agents do the day-to-day
work of small businesses, and a lot of that work lives in Word. It is a stand-alone project:
nothing in it depends on Foundworks.

## License

[Apache-2.0](LICENSE).
