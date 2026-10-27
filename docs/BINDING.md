# Python binding (`quillwork`)

The `quillwork` wheel is the Rust core behind a PyO3 module. Host applications and the
`quillwork-server` package (`python/quillwork_server`) consume it as a pinned wheel.

- Crate: `crates/quillwork-py` (PyO3 0.30.1, maturin 1.15, mixed Rust/Python layout).
- Wheel: `abi3`, CPython **3.12+**, so there is one wheel per platform. DESIGN §2 first said
  `abi3-py310`. It is 3.12 because the first host application runs 3.12, and because the 3.12
  limited API is the floor we test. Lower it only if a host needs 3.10.
- Typed: `py.typed` plus `_native.pyi` stubs. `mypy --strict` is clean on the package and the example.

## Layout

```
crates/quillwork-py/
  Cargo.toml          cdylib + rlib named `quillwork`; pyo3 without abi3 (see Build)
  build.rs            rpath for `cargo test` against a framework Python on macOS
  pyproject.toml      maturin backend; module `quillwork._native`; feature pyo3/abi3-py312
  src/lib.rs          #[pymodule] _native
  src/errors.rs       core Error -> Python exception: the one mapping table
  src/package.rs      Limits, Relationship, Package
  src/xml.rs          XmlLimits, XmlDocument, conventional_namespaces()
  src/document.rs     JsonDocument: the JSON op seam (stub)
  python/quillwork/   __init__.py, errors.py, ns.py, document.py, _native.pyi, py.typed
  examples/replace_text.py
  tests/              pytest
```

`pyproject.toml` lives in the crate, not at the top level, because DESIGN §15 reserves the top-level
`python/` for the MCP server and the reference service.

## API

Each layer sits on top of the one before it. Agents use only the third. The first two are for engine
work and adapters.

**`Package`**: the zip (OPC) layer. Saving an unmodified package returns the input bytes exactly. After
edits, only the changed entries are re-written.

| | |
|---|---|
| `Package.open(data, limits=None)` | Opening can raise `EncryptedError`, `LimitError` or `BadPackageError`. |
| `names()`, `in`, `len()` | List and count the parts, in archive order. |
| `part(name)` / `set_part` / `add_part(name, data, content_type)` / `remove_part` | Read and write raw part bytes. A missing part raises `NotFoundError`. |
| `rels_of(source="")` → `list[Relationship]`, `resolve(source, rel)`, `content_type_of(name)` | Follow relationships and content types. |
| `xml(name, limits=None)` → `XmlDocument`, `set_xml(name, doc)` | Parse a part and write it back, with no intermediate Python `bytes`. |
| `is_modified`, `save()` | Check for changes and serialise. |

`Limits(...)` and `XmlLimits(...)` take keyword arguments only. Any limit you leave out uses the core
default.

**`XmlDocument`**: one XML part, lossless. Nodes are `int` handles into the core's arena.

| Group | Methods |
|---|---|
| Parse and serialise | `parse`, `to_bytes`, `node_to_bytes` |
| Document info | `is_modified`, `node_count`, `root`, `document_node` |
| Node info | `kind` (a `Literal`), `name`, `local_name`, `prefix`, `namespace`, `matches(node, ns, local)`, `lookup_namespace` |
| Navigation | `parent`, `first_child`, `last_child`, `next_sibling`, `prev_sibling`, `children`, `child_elements`, `descendants`, `ancestors`, `find_child`, `find` (first descendant), `find_all` |
| Attributes | `attr`, `attr_ns`, `attrs`, `set_attr`, `set_attr_ns`, `remove_attr`, `set_name` |
| Text | `text` (text, cdata and comment nodes only), `text_content` (all text below an element), `set_text` (replaces an element's children with one text node), `source_span` |
| Structure | `create_element(qname)`, `create_text`, `create_comment`, `clone_subtree`, `append_child`, `prepend_child`, `insert_before`, `insert_after` (each of these moves a node if it is already attached), `detach`, `remove` |

`quillwork.ns` holds the OOXML namespace constants (`W`, `R`, `WP`, `MC`, ...), `CONVENTIONAL`
(prefix → URI, in the same order as the core) and `RT_OFFICE_DOCUMENT`. A test checks them against
the core's list.

**`Document`**: the op language in `spec/`, carried as JSON, over the core's edit engine
(`quillwork_core::engine::Engine`).

| | |
|---|---|
| `Document.open(data, limits=None, *, sidecar=None, version=None)` | Open a .docx. `sidecar` (a `str` or a dict) restores the anchors, change sets and version a previous save wrote. The document opens at `max(version, the sidecar's version)`, and at 1 for a fresh upload. A `version` below the sidecar's raises `BadVersionError`; an unreadable sidecar raises `BadPackageError`. |
| `version` | The current version. Every committed transaction adds one. |
| `apply(tx)` → OpResult dict, `apply_text(tx)` → str | Apply a Transaction (`str` or dict) atomically. An `ok: false` result raises the matching `QuillworkError` subclass with the spec error dict in `.error`; `apply_text` returns the §9 text form of the result or the error instead of raising. |
| `save()` → bytes, `sidecar()` → str, `save_with_sidecar()` → `(bytes, str)` | Persist. Keep the sidecar next to the .docx and pass it back to `open`. `sidecar()` is cached from the last save until the next apply, so `save()` then `sidecar()` serialises once. |
| `change_sets()` → list of view-model `ChangeSet` dicts | The change-set log, rejected and accepted ones included. |
| `render(handles)` → list[str] | The markup-mode projection line of each block handle (`{++ins++}`, `{--del--}`, author marks). |
| `set_agent(author)`, `set_now(unix_secs=None)` | The default revision author (a transaction's `author` wins) and a fixed revision clock for reproducible output. Neither is stored in the sidecar: set them again after `open`. |
| `read_op(req)` → ReadResult dict | Any read op of the catalogue (spec/projection.md §8), as a `str` or a dict. Errors raise like `apply`: unknown keys and bad values raise `InvalidOpError` with `path` and `usage`, a missing anchor `NotFoundError`. |
| `read(selection=None, mode="accepted", budget=None)`, `find(query, *, regex, story, mode)`, `lint()`, `help(name=None)` → str | The text read ops: the `text` of the result (`§ lint: none` for a clean document). |
| `get_block(anchor)`, `list_changes(*, change_set, author, status)`, `list_comments(*, open_only)`, `list_images()`, `get_image(image=None, *, asset=None)`, `list_fields()` → dict | The data read ops: the `data` of the result. `list_changes` and `list_comments` are what a review tool reads before it accepts, rejects or replies. `get_image` takes exactly one of `image` and `asset` (`TypeError` otherwise). |
| `view_model()` → dict | The view model. After the first call every `apply` result's `patch` is the full patch from it. |

**Versions.** The spec numbers an upload 1; the core engine opens a file without a sidecar at 0 and
has no version setter. The binding bridges this without touching core: it writes the target version
into the sidecar it hands `Engine::open` (wrapping a bare anchors sidecar, or an empty one, which
numbers the blocks exactly as a fresh mint does; a Rust test checks this over the public corpus), and
the engine fast-forwards to it. That is also how a host reopens an import or restore at `head + 1`
with an older sidecar.

**`limits`** is a precheck: the package is opened with them first, then the engine opens it again with
the core defaults (`Engine::open` takes no limits).

**INVALID_OP envelopes.** The core leaves `usage` (and sometimes `path`) out of some INVALID_OP
errors: request-level parse errors and some argument errors raised while an op runs. The spec
requires both, so `apply` fills them in (the op's signature from `quillwork_core::ops::usage`, or the
Transaction's) before the envelope crosses the boundary. Better fixed in core.

The seam is fixed now. Python passes a JSON string to `JsonDocument.apply_json`, `read_json` or
`view_model_json`; the Rust side (serde) parses it, validates it and returns a JSON envelope; Python
raises `error_from_dict(envelope["error"])` when `ok` is false. Because Rust owns every shape, there is
no per-field Python class to keep in sync, and the WASM binding will use the same strings.

The tests validate every `spec/examples/ops/*.json` against `ops.schema.json`, send each one through the
seam, and check every envelope that comes back against `error.schema.json`. Validation happens in the
tests only: jsonschema is a test extra, not a runtime dependency.

## Errors

`quillwork.errors` has one base class, `QuillworkError`, and one subclass per spec code (14 codes in
`BY_CODE`). Each exception has `.code`, `.message`, `.retryable`, `.reason`, `.version`, `.op_index`,
`.op`, `.anchor`, `.offset` and `.limit`, plus `.error` (the full error object) and `.get(field)` for
fields specific to one code (`candidates`, `path`, ...).

The native layer maps every core `Error` variant in one place, `src/errors.rs`, which has a unit test
per variant:

| Core variant | Python class | `code` |
|---|---|---|
| `Limit` | `LimitError` | LIMIT |
| `Package` | `BadPackageError` (also a `ValueError`) | BAD_PACKAGE |
| `Xml`, `XmlParse{offset}`, `XmlDoctype{offset}` | `XmlError` > `XmlParseError` > `XmlDoctypeError`, all `BadPackageError` | BAD_PACKAGE |
| `Encrypted` | `EncryptedError` (a `ProtectedError`), with `reason="encrypted"` | PROTECTED |
| `NotFound` | `NotFoundError` (also a `LookupError`) | NOT_FOUND |
| `XmlNode`, `XmlValue` | `XmlNodeError`, `XmlValueError` (both `InvalidOpError`, also a `ValueError`) | INVALID_OP |
| `LimitExceeded{name,max,actual}` | `LimitError` with `.limit == {name, max, actual}` | LIMIT |
| `Style`, `Wml`, `Anchor` (incl. a bad sidecar) | `BadPackageError` | BAD_PACKAGE |
| `Edit{code}` (engine errors outside a transaction, e.g. `open`) | the class for `code` (`BadVersionError`, `StaleAnchorError`, ...) | `code` |
| `Other`, plus any variant added later | `InternalError` | INTERNAL |

Errors from `apply` don't go through this table: the engine answers with a spec error envelope, and
Python raises `error_from_dict(envelope["error"])`, so every spec field (`current`, `candidates`,
`anchor`, `path`, `usage`, ...) arrives as the engine wrote it.

A negative node id raises Python's `OverflowError`, which PyO3 raises before the core sees the call. An
id that is out of range or deleted raises `XmlNodeError`.

## GIL, copies, threads

- **GIL.** The GIL is released (`py.detach`) around all of the heavy work: `Package.open`, `part`,
  `set_part`, `add_part`, `xml`, `set_xml`, `save`, `XmlDocument.parse`, `to_bytes`, and the heavy
  `Document` calls: `open`, `apply`, `apply_text`, `save`/`sidecar`/`save_with_sidecar` and `render`.
  `tests/test_gil.py` proves the GIL is released for parse, to_bytes, open, part, xml and save, and for
  `Document.open`, `apply` and `save`. It does this by showing that a second Python thread runs during the call, and a control
  case shows the probe fails for `find_all`. Small accessors keep the GIL: the call costs more than the
  work.
- **Copies.**
  - `open` copies the input once, because the core takes `Vec<u8>`. A core source of
    `Arc<[u8]>`/`Bytes` would make it zero-copy; it is not worth doing until profiles say so.
  - `part`, `save` and `to_bytes` copy once into a `bytes` object.
  - `xml(name)` parses straight from the core's buffer.
- **Threads.** Separate `Package` and `XmlDocument` objects run in parallel; a test checks that
  concurrent parses agree. Using one object from two threads is serialised by PyO3's borrow flag. If a
  mutation overlaps a call that has released the GIL, PyO3 raises `RuntimeError: Already borrowed`
  rather than racing. Use one object per thread, or a lock.

## Ergonomics verdict (DESIGN §2 criterion 5): GO

The DESIGN §2 probe is `examples/replace_text.py`. It replaces the first run's text **as a tracked
change, keeping `w:rPr`**:

1. Clone the run and drop everything except `rPr`.
2. Add a new `w:t`.
3. Wrap the original run in `w:del`, renaming its `w:t` to `w:delText`.
4. Add a `w:ins` after it, with a fresh revision id, author and date.

`--direct` instead overwrites the text in place. The edit logic is about 35 lines, plus helpers that
find the main part and allocate a revision id. The example checks three things:

- every other zip entry is byte-identical (the local header and the compressed bytes);
- `w:ins` follows `w:del`, and their texts match;
- both revision runs carry identical `w:rPr` bytes.

It passes on all 8 corpus files (2 synthetic and 6 private) in both modes, and `mypy --strict` is clean.

It reads like ordinary Python. Finding things, moving nodes and serialising all work without surprises.
Errors are normal Python exceptions with spec codes, and the stubs make an IDE useful. Nothing in it
needed `unsafe` reasoning or knowledge of the arena.

Six things were clunky. Each could change without touching the core design:

1. **Int handles.** Every call is `doc.verb(node, ...)`, so you write `doc.matches(run, ns.W, "r")`
   rather than `run.is_(W.r)`, and mypy cannot tell a node id from any other `int`.
   - Cheap fix: a `NodeId = NewType("NodeId", int)` in the stubs, at no runtime cost.
   - Fuller fix: a pure-Python `Node` view (`node.parent`, `node.children`, `node.is_(ns, local)`)
     over the same handles. Python objects cannot hold borrows into the arena (DESIGN §2), but a
     `(doc, id)` pair is fine.
2. **`text` versus `text_content`.** `set_text(element, s)` works on an element, but `text(element)`
   returns `None`; you need `text_content`. The core semantics are sound (leaf value versus subtree
   text), but the names invite the mistake. Rename `text` to `value`, or make `text` behave like
   `text_content` for elements.
3. **Lookups and creation use different name forms.** Lookups take `(ns, local)`, but creation takes a
   qualified name, so the example reads the run's prefix (`doc.prefix(run) or "w"`) to build
   `"w:del"`. Add `create_element_ns(ns, local)` that picks the prefix in scope at insert time.
4. **Package plumbing.** Finding the main part takes a relationship walk (`rels_of` →
   `RT_OFFICE_DOCUMENT` → `resolve`). Add `Package.main_document()` and
   `rels_of(source, rel_type=...)`.
5. **No revision-id allocator.** The example scans the whole part for the highest `w:id`. The op
   engine needs one anyway (it should be per package and cover comments and bookmarks), and it belongs
   in the core.
6. **Moving versus cloning.** Insert calls move a node that is already attached. That is convenient,
   but it is easy to forget that `append_child(deleted, run)` takes `run` out of the paragraph. The
   docstrings now say so.

None of these blocks the design. Agents never see this layer; they use `Document` and the op JSON.
Items 1 to 4 are binding-only changes worth making before `quillwork-mcp` builds on it. Item 5 is
core work.

## Gaps found in the spec while binding

- **LIMIT structure.** The core's `Error::LimitExceeded` carries `{name, max, actual}` and fills
  `.limit`; the older `Error::Limit` (the zip/XML layers) is still a bare string, so `.limit` is `None`
  there.
- **INVALID_OP without `usage`.** See "INVALID_OP envelopes" above.
- **The agent and clock are not in the sidecar.** A reopened document signs revisions as the default
  agent ("AI") until `set_agent` is called again.
- **NOT_FOUND has no `what` for a part.** A native missing-part error carries no `what` (the enum is
  anchor, text, comment, change_set, revision and asset). That is fine for the op layer, which never
  exposes parts, but worth knowing.
- **Native errors carry no `version`.** It is `None` because there is no document version below
  `Document`.

## Build, test, wheels

Use a project venv; nothing needs to be installed globally.

```sh
uv venv --python 3.12 .venv && uv pip install --python .venv/bin/python pytest jsonschema mypy
VIRTUAL_ENV=$PWD/.venv uvx maturin develop --uv -m crates/quillwork-py/Cargo.toml   # editable install
.venv/bin/python -m pytest crates/quillwork-py/tests                                   # 121 without the private corpus
cd crates/quillwork-py && ../../.venv/bin/python -m mypy --strict examples/replace_text.py python/quillwork

cargo test --workspace                       # also runs the errors.rs unit test, linking libpython
cargo clippy -p quillwork-py --all-targets -- -D warnings

uvx maturin build --release -m crates/quillwork-py/Cargo.toml --out dist   # host wheel
```

- **abi3 in builds only.** `abi3-py312` is turned on by maturin (`[tool.maturin] features`) and not in
  `Cargo.toml`. A plain `cargo test` therefore builds against whatever `python3` is first on `PATH`
  (macOS ships 3.9), instead of failing with "minimum Python version 3.12 higher than the interpreter".
  Set `PYO3_PYTHON=/path/to/python3.12` to test against 3.12. `build.rs` adds the libpython rpath, so
  the test binary loads a framework Python on macOS. It does nothing for maturin builds.
- **Private corpus.** It is resolved the same way as in the Rust tests: from
  `$QUILLWORK_PRIVATE_CORPUS`, else from a `quillwork-private-corpus` folder in a parent directory
  (the root itself when it holds `.docx` files, else its first subfolder that does). When found, its files join the round-trip, edit-locality and example tests, labelled
  `private-<index>`. Assertions print booleans and part names, never document text.
- **CI** (`.github/workflows/wheels.yml`). On `v*` tags, or when run by hand, it builds a linux x86_64
  `manylinux_2_28` wheel and a macOS arm64 wheel with `PyO3/maturin-action`, then uploads them as
  workflow artifacts. Actions are pinned to commit SHAs, and nothing is published. DESIGN §15 also wants
  Linux arm64 and Windows wheels; add those rows to the matrix when a consumer needs them.
- **Local manylinux without the action.**

  ```sh
  docker run --rm --platform linux/amd64 -v "$PWD":/io -w /io quay.io/pypa/manylinux_2_28_x86_64 bash -euc '
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.98.0
    . $HOME/.cargo/env && /opt/python/cp312-cp312/bin/pip install maturin==1.15.0
    /opt/python/cp312-cp312/bin/maturin build --release --manylinux 2_28 -i /opt/python/cp312-cp312/bin/python \
      -m crates/quillwork-py/Cargo.toml --out dist --target-dir target/manylinux'
  ```

Wheel sizes (0.0.1, release profile: fat LTO, stripped) are in the table below. DESIGN §2 estimated
0.3 to 0.6 MB.

| Wheel | Size |
|---|---|
| `cp312-abi3-macosx_11_0_arm64` | 434 KB (855 KB `.so`) |
| `cp312-abi3-manylinux_2_28_x86_64` | 489 KB (952 KB `.so`); its pytest run in the container: 111 passed, 1 skipped (mypy) |
