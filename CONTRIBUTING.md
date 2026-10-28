# Contributing to Quillwork

Thanks for taking the time. Bug reports, reproducers, fixes and new ops are all welcome.

## Never attach a confidential document

Quillwork exists to edit contracts, letters and other business documents, and those are
usually confidential. **Do not attach a real document to an issue or a pull request, and never
commit one.** Reproduce the problem on a document you made yourself:

- `python examples/make_sample.py sample.docx` writes a small synthetic contract,
- `corpus/synthetic/gen.py` generates larger ones,
- or build a minimal file in Word or LibreOffice that shows the same structure (a numbered list,
  a table with merged cells, a tracked change, and so on).

If a bug only shows up on a document you cannot share, describe its structure instead: which
parts the package has, the XML around the failing anchor with the text replaced, and the error.
The test suites can read a private folder of your own documents through
`QUILLWORK_PRIVATE_CORPUS`. Those tests skip when the folder is absent and may only report
numbers, never document text.

## Layout

| Path | Language | What |
|---|---|---|
| `crates/quillwork-core` | Rust | the engine |
| `crates/quillwork-py` | Rust + Python | the `quillwork` Python binding (PyO3, maturin) |
| `python/quillwork_server` | Python | `quillwork-server`: agent tools, MCP and HTTP |
| `js/viewer` | TypeScript | `@quillwork/viewer` |
| `spec/` | JSON Schema + Markdown | the contract every binding implements, with its own tests |
| `tools/oracle` | Python | independent validity and render checks |
| `bench/` | Python | the fidelity and token benchmark quoted in the README |

`docs/DESIGN.md` records the design and the reasoning behind it. Read it before changing the
tree, the splicer or the anchor scheme.

## Build and test

You need Rust 1.88 or newer, [uv](https://docs.astral.sh/uv/), Python 3.12 and Node 22.12 or
newer. CI (`.github/workflows/ci.yml`) runs exactly the commands below, so if they pass locally
your pull request should pass too.

The tests look for a private corpus and skip those cases when there is none. To make sure a run
only uses the public files, point the variable at an empty directory:

```sh
mkdir -p /tmp/qw-empty && export QUILLWORK_PRIVATE_CORPUS=/tmp/qw-empty
```

### Rust

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

### Python binding, spec and server

```sh
uv venv --python 3.12 .venv && source .venv/bin/activate
uv pip install maturin pytest jsonschema mypy python-docx tiktoken
maturin develop -m crates/quillwork-py/Cargo.toml
uv pip install -e 'python/quillwork_server[test]'

pytest crates/quillwork-py/tests spec/tests
mypy --strict crates/quillwork-py/python/quillwork crates/quillwork-py/examples

cd python/quillwork_server
pytest
ruff check . && ruff format --check .
mypy
```

### Viewer

```sh
cd js/viewer
npm ci
npm run typecheck && npm test && npm run build
```

### Oracle

```sh
uv run --project tools/oracle pytest tools/oracle/tests
```

The validator tests need Docxodus, a .NET Open XML validator found through `QUILLWORK_DOCXODUS`,
and skip without it. Render checks need LibreOffice.

### Benchmark

```sh
python bench/fidelity.py
```

If you change anything that can move the numbers in the README's Measurements section, rerun
the benchmark and update `bench/results.md` and the README in the same pull request.

## Pull requests

- Keep each pull request to one change, with a test that fails without it.
- A new op or a change to the op language starts in `spec/`: the schema, an example, and an
  entry in `CHANGELOG.md`. The bindings follow the spec, never the other way round.
- Untouched bytes must stay untouched. A change that makes a no-op save or an unrelated part
  differ from the input is a regression, even if Word still opens the file.
- Commit messages use `<type>(<scope>): <summary>` in lower case, for example
  `fix(ops): keep run properties on a split paragraph`. Types in use: `feat`, `fix`, `docs`,
  `test`, `chore`, `style`, `refactor`. The body explains why.
- `git blame` should skip the bulk formatting commit: run
  `git config blame.ignoreRevsFile .git-blame-ignore-revs` once.

By contributing you agree that your contribution is licensed under the Apache License 2.0, the
same licence as the project (see section 5 of [LICENSE](LICENSE)).

## Conduct

Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
