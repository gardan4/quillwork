# Quillwork spec

The language-neutral contract every Quillwork binding implements: the Rust core, the Python and
WASM bindings, the TypeScript viewer and the MCP server. If a binding and this directory disagree,
the binding is wrong. Design background: [docs/DESIGN.md](../docs/DESIGN.md) §5, §6, §8, §11.

`spec_version`: **0.1.0**

## What's here

| Path | What |
|---|---|
| [anchors.md](anchors.md) | anchor grammar (ABNF + regex), serial rules, positional anchors, validity over time |
| [projection.md](projection.md) | the text projection agents read: grammar, inline notation, tracked changes, paging, result and error rendering, a worked example |
| [errors.md](errors.md) | every error code: when it fires, what it carries, how an agent recovers |
| [schema/common.schema.json](schema/common.schema.json) | shared defs: anchors, ids, versions, guards, units, projection lines, lint findings |
| [schema/ops.schema.json](schema/ops.schema.json) | the op catalogue: one `Transaction` of mutating ops, or one read op; `OpResult`, `ReadResult` |
| [schema/view-model.schema.json](schema/view-model.schema.json) | the resolved, render-ready JSON tree the viewer draws |
| [schema/patch.schema.json](schema/patch.schema.json) | the viewer patch each committed transaction emits |
| [schema/error.schema.json](schema/error.schema.json) | the error response |
| [tools/](tools/) | the 8 agent tools (neutral JSON) and `export.py`, which emits Anthropic, OpenAI and MCP shapes |
| [examples/](examples/) | invented documents and payloads; every one is validated by the tests |
| [tests/test_spec.py](tests/test_spec.py) | the spec's own conformance tests and token report |

All schemas are JSON Schema 2020-12 with `$id` under `https://quillwork.example/spec/v0/`; they
reference each other by relative `$ref` (`common.schema.json#/$defs/Anchor`). Load them into one
registry keyed by `$id`.

## Two layers: ops and tools

**Ops** ([schema/ops.schema.json](schema/ops.schema.json)) are the full catalogue, 38 mutating and
11 read ops, precise and verbose. The engine API (`apply(transaction)`, `read(op)`) takes ops. One
`Transaction` is atomic: every op applies or none does, and a committed transaction is one version
and (in track mode) one change set.

**Tools** ([tools/](tools/)) are what an LLM sees: 8 small schemas, 1,277 tokens together, that
compile to ops. Common work has its own tool; everything else goes through `advanced_op`, whose
`help` op lists the catalogue on demand. A tool call compiles to exactly one transaction or one
read.

| Tool | One line | Compiles to |
|---|---|---|
| `read_document` | read anchored lines: header + outline + first window, a range, or the next window | `read` |
| `find` | search every story, returns matching lines with anchors | `find` |
| `edit` | text edits as one tracked change set | `replace` → `replace_text` (on a cell anchor with `find` null: `set_cell`); `insert_after` / `insert_before` → `insert_paragraph`; `delete` → `delete_block` (`find` null) or `replace_text` with `replace: ""`; `move_after` / `move_before` → `move_block`; `split` → `split_paragraph` (`at_text` = `find`); `merge` → `merge_paragraphs` (`first` = anchor, `second` = the next block) |
| `format` | formatting changes, tracked | `marks` → `set_text_format`; `style`, `align` → `set_paragraph`; `list` → `set_list` (`restart:<n>` → `restart_numbering`); `level` → `change_level` |
| `comment` | add, reply, resolve, reopen, delete | `add_comment`, `reply_comment`, `resolve_comment` (`done: true`), `resolve_comment` (`done: false`), `delete_comment` |
| `table` | insert a table, insert or delete rows | `insert_table`; `insert_rows` → one `insert_row` per row; `delete_row` |
| `review` | accept or reject tracked changes | `accept`, `reject` (`cs_7` → `change_set`, `r12` → `revision_ids`, `author:<name>` → `author`, `all` → `all`) |
| `advanced_op` | any other catalogue op by name, with its args | that op (read ops ignore `base_version`) |

Mutating ops reached only through `advanced_op`: `insert_break`, `insert_footnote`, `set_hyperlink`,
`insert_field`, `insert_column`, `delete_column`, `merge_cells`, `split_cell`, all image ops
(`insert_image`, `replace_image`, `resize_image`, `set_alt_text`, `delete_image`, `anchor_image`),
`compare`, `fill`, `set_properties`, `set_track_changes`. Read ops reached only through it:
`outline`, `get_block`, `list_changes`, `list_comments`, `list_images`, `get_image`, `lint`,
`list_fields`, `help`.

Tool schemas follow the intersection of OpenAI strict mode, Anthropic `input_schema` and MCP
`inputSchema`: object root, every property listed in `required` (optional = nullable),
`additionalProperties: false`, no `format`/`pattern`/`min*`/`default`, nesting ≤ 5. Value checks the
tool schema can't express happen when the call compiles to ops and come back as `INVALID_OP`.

Emit the provider shapes with:

```sh
python spec/tools/export.py anthropic          # [{name, description, input_schema}]
python spec/tools/export.py openai             # Chat Completions, strict: true
python spec/tools/export.py openai-responses   # Responses API, strict: true
python spec/tools/export.py mcp                # [{name, description, inputSchema}]
```

## Versioning

Every request may and every response must carry `spec_version` (semver). The engine accepts any
request whose major version equals its own and whose minor version is ≤ its own.
Malformed or incompatible versions return `INVALID_OP` at `/spec_version` before the
request runs. Prerelease and build identifiers follow SemVer syntax; compatibility uses
only the stated major/minor rule. Omitting `spec_version` uses the engine's current spec.

Document `version` is a separate, nonnegative optimistic-concurrency counter. A fresh Rust
`Engine::open` starts at 0; Python `Document.open` and service uploads start at 1. Each
committed transaction increments the current counter, and reopening a Rust sidecar preserves
its stored counter, including 0. Hosts should use the version returned by their API rather
than assume an initial value. Python's existing minimum-1 upload convention remains unchanged.

- **Patch** (0.1.x): wording, examples, descriptions, tests. No payload that was valid becomes
  invalid or vice versa.
- **Minor** (0.x.0): additions that old clients can ignore: new ops, new optional or nullable
  fields, new enum values in responses (error codes, lint rules, reasons, flags, block kinds),
  new projection markers. Clients must ignore unknown response fields and treat an unknown error
  code like `INTERNAL`.
- **Major** (x.0.0): anything else: a removed or renamed op or field, a changed meaning, a new
  required request field, a changed anchor grammar.

While the major version is 0, minor versions may still break; each break is listed in the commit
that makes it. The `$id` path segment (`/v0/`) changes with the major version only. Tool schemas
are versioned with the spec: a host caching tools in a prompt prefix re-exports them per minor
version.

## How bindings conform

A binding conforms to spec version X.Y.Z when:

1. **Schemas.** Every payload it emits validates against these schemas: view model, patch, results,
   errors. Every valid request is accepted (or fails with a documented error code, never a crash).
2. **Examples.** It round-trips the examples: parsing then serialising each file in `examples/`
   yields equal JSON, and requests in `examples/ops/` parse into its native types.
3. **Projection.** Given the document behind [examples/view-model.json](examples/view-model.json),
   its projection is byte-identical to
   [examples/projection-accepted.txt](examples/projection-accepted.txt) and
   [examples/projection-markup.txt](examples/projection-markup.txt). (The corpus document for this
   arrives with the engine's test fixtures; until then, render the view model.)
4. **Anchors.** It accepts every `valid` string in [examples/anchors.json](examples/anchors.json)
   and rejects every `invalid` one, per kind.
5. **Errors.** It returns the documented code, with the required fields, for each situation in
   [errors.md](errors.md). A failed transaction leaves the document version unchanged.
6. **Tools.** If it exposes agent tools, it exposes these 8 with these schemas (exported, not
   hand-copied), and compiles them to ops as in the table above.

Bindings should vendor this directory at a tagged commit and run its tests in CI, plus their own
round-trip tests against `examples/`.

## Examples: directory convention

The tests find the schema for each example from where it lives, so there is no manifest:

| Location | Validates against |
|---|---|
| `examples/view-model.json` | `view-model.schema.json` |
| `examples/patch.json` | `patch.schema.json` |
| `examples/ops/<op>.json` | `ops.schema.json`; for mutating ops a `Transaction` whose first op is `<op>`, for read ops the op itself. `_transaction-multi.json` mixes several ops |
| `examples/errors/<CODE>.json` | `error.schema.json`, one per code |
| `examples/results/op-result*.json` | `ops.schema.json#/$defs/OpResult` |
| `examples/results/read-result*.json` | `ops.schema.json#/$defs/ReadResult` |
| `examples/tool-calls/<tool>.json` | that tool's `parameters` |
| `examples/anchors.json` | the `common.schema.json` def named by each key |
| `examples/projection-*.txt` | the projection grammar; anchors must exist in the view model (the prose sample is standalone) |

A file anywhere else in `examples/` fails the tests. Every op and every error code must have an
example. All text is invented (a Dutch childcare employment contract); no example may contain a
real client document.

## Token budget

DESIGN §1 targets, measured with tiktoken `o200k_base` by `pytest spec/tests -s`:

| Target | Budget | Measured (0.1.0) |
|---|---|---|
| tool count | ≤ 8 | 8 |
| tool schemas, all together | ≤ 2,000 | 1,277 neutral; 1,287 Anthropic / MCP, 1,359 OpenAI Chat, 1,335 OpenAI Responses |
| first call (header + outline + first window + appendix) | ≤ 8,000 | 645 (accepted), 661 (markup), 1,497 (prose sample) |
| body tokens per page (page = 1,800 plain characters) | ≤ 550 | 505 (prose sample) |
| ratio to the plain text | ≤ 1.25 | 1.21 (prose sample) |
| anchor overhead | ≤ 10% | 14.4% (prose sample): **misses the target** |

The two feature-dense samples (accepted, markup) run 2.2× the plain text by design: they exercise
every marker on a third of a page. The prose sample is the representative measure. Anchor overhead
is the share of body tokens that disappears when the `[anchor kind]` prefixes are removed but the
list labels kept; on short paragraphs a `[b10c #3.1] ` prefix costs 5–6 tokens. Options to close
the gap, not taken in 0.1.0 because each costs readability or stability: drop the brackets (9.7%),
shorter body serials, or anchors only on every other line of a run.

## Running the tests

```sh
uv venv && uv pip install jsonschema tiktoken pytest   # once, at the repo root
.venv/bin/pytest spec/tests -q                         # add -s for the token report
```
