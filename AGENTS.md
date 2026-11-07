# AGENTS.md

Instructions for coding agents working in this repository. Humans: see
[CONTRIBUTING.md](CONTRIBUTING.md), which this file condenses. When the two disagree,
CONTRIBUTING.md wins.

## Hard rules

- Never commit, attach or paste a real document. Business documents are usually confidential.
  Use `python examples/make_sample.py <out.docx>`, `corpus/synthetic/gen.py` or a file you build
  yourself.
- Tests may read a private corpus through `QUILLWORK_PRIVATE_CORPUS`. They may only report
  numbers, never document text. For a public-only run: `mkdir -p /tmp/qw-empty && export QUILLWORK_PRIVATE_CORPUS=/tmp/qw-empty`.
- Untouched bytes stay untouched. A no-op save or an unrelated part that differs from the input
  is a regression, even if Word still opens the file.
- A new op or a change to the op language starts in `spec/` (schema, example, `CHANGELOG.md`
  entry). Bindings follow the spec.
- Read `docs/DESIGN.md` before changing the tree, the splicer or the anchor scheme.

## Layout

| Path | What |
|---|---|
| `crates/quillwork-core` | Rust engine |
| `crates/quillwork-py` | `quillwork` Python binding (PyO3, maturin) |
| `python/quillwork_server` | `quillwork-server`: agent tools, MCP and HTTP |
| `js/viewer` | `@quillwork/viewer` (TypeScript) |
| `spec/` | the contract every binding implements, with its own tests |
| `tools/oracle` | independent validity and render checks |
| `bench/` | fidelity and token benchmark quoted in the README |

## Checks

CI (`.github/workflows/ci.yml`) runs these. Run the ones for the parts you touched.
Toolchain: Rust 1.88 or newer, uv, Python 3.12, Node 22.12 or newer.

```sh
# Rust
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Python binding, spec and server (in a uv venv, see CONTRIBUTING.md for the install)
pytest crates/quillwork-py/tests spec/tests
mypy --strict crates/quillwork-py/python/quillwork crates/quillwork-py/examples
(cd python/quillwork_server && pytest && ruff check . && ruff format --check . && mypy)
ruff check examples bench

# Viewer
(cd js/viewer && npm ci && npm run typecheck && npm test && npm run build)

# Oracle
uv run --project tools/oracle pytest tools/oracle/tests
```

`pre-commit install` runs the fast ones (rustfmt, clippy, ruff, gitleaks) before each commit.
If a change can move the README's Measurements numbers, rerun `python bench/fidelity.py` and
update `bench/results.md` and the README in the same pull request.

## Pull requests

- One change per pull request, with a test that fails without it.
- Commit messages: `<type>(<scope>): <summary>` in lower case, types `feat`, `fix`, `docs`,
  `test`, `chore`, `style`, `refactor`. The body explains why.
- User-facing changes get a `CHANGELOG.md` entry under Unreleased.
- Contributions are licensed under Apache-2.0.
