# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Version numbers follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html), with the usual 0.x caveat: until 1.0,
any release may change the op language, the projection or the sidecar format, and such changes
are listed under **Changed**.

## [Unreleased]

### Added

- The validator checks child order in `settings.xml` and `styles.xml` against the schema, as
  warnings (`order.child_order`).

### Fixed

- Story text in the accept-all view runs a paragraph whose mark is deleted on into the next
  instead of keeping an empty line, and reject-all does the same for an inserted mark. It now
  matches the per-paragraph view.

## [0.0.4] - 2026-10-28

### Added

- Experimental `pml::Presentation` / `quillwork.Presentation` for existing transitional PPTX
  decks: anchored text discovery, atomic run-preserving replacement in shapes, groups, tables
  and notes, literal Python template fill, and original/candidate hashes with text review.
  Includes a synthetic business template, walkthrough, transaction schema and documented host
  approval workflow. Slide creation, layout reflow, images/charts and reference-server/viewer
  integration are outside this first API.

### Changed

- Validate optional Word request `spec_version` as SemVer and reject incompatible major/minor
  versions before execution. Shared schemas now accept Rust's zero-origin counters without
  changing Python/service version-1 uploads or historical sidecar counters.
- Locked content-control refusals consistently return `PROTECTED` with reason `locked_control`.
  Enforced document restrictions preserve supported comment/tracked-edit categories and refuse
  unresolved forms, permission exceptions and restricted-style policies.

- Rust view-model `Marks.b`, `i` and `caps` are optional booleans: absence inherits;
  `false` explicitly disables inherited formatting. Run color may be `auto` to reset an
  inherited custom color; the viewer and schema implement the same convention.

### Fixed

- View-model/viewer formatting preserves explicit off values, effective inherited decorations,
  automatic color resets and default-style overrides. Formatting `false` requests turn off
  inherited properties while `null` removes direct formatting; superscript/subscript resets
  preserve the opposite alignment.

- Historical Word revision IDs stay reserved across reopen, and resolved change sets cannot
  select unrelated imported revisions that reuse their numeric IDs.
- Changes to table content or structure invalidate table/row/cell concurrency guards.
- Nonempty text for a vertically merged continuation cell is refused atomically instead of
  silently discarded.
- Saving an insertion and then rejecting it removes it from exported bytes as well as the
  live view, including synchronization into a reused package.
- Table and review mutations respect locked content controls and enforced document policies.
- The Word engine refuses non-Word main parts; sidecar versions restore in constant time,
  and exhausted version/change-set counters are refused before mutation.

- The reference service primes cold-writer patch baselines and returns coherent, continuous
  patch feeds bounded by the sampled head version.
- Agent table tools preserve requested row order. Idle SSE waits use separate bounded worker
  capacity and release actual workers on disconnect/cancellation.
- The locality oracle treats `[Content_Types].xml` as a literal allowed part name.
- `quillwork-server` without an API key answers only to loopback `Host` and `Origin` values
  (plus `--allowed-host` / `QUILLWORK_ALLOWED_HOSTS`), so a page on another site cannot read
  or edit local documents through DNS rebinding or a cross-site request. `create_app` takes
  the same check as `allowed_hosts`.

## [0.0.3] - 2026-10-27

The first release on PyPI: `pip install quillwork quillwork-server`. The 0.0.2 wheels of
`quillwork` reached PyPI, but its source distribution and `quillwork-server` did not. No code
changes since 0.0.2.

### Fixed

- The `quillwork` source distribution carries `LICENSE` and `NOTICE` at its root, where its
  metadata says they are; PyPI rejected the 0.0.2 one without them.

## [0.0.2] - 2026-10-27

First public release. Wheels for `quillwork` (CPython 3.12+ abi3, macOS arm64 and Linux x86_64)
and `quillwork-server` are attached to the GitHub release. 0.0.1 was an internal build and was
never published.

### Added

- `quillwork-core`: raw zip splicer and raw-span XML tree, so a no-op save is byte-identical
  and an edit rewrites only the parts it touches; Word model, stable base32 anchors with an
  optional sidecar, the CriticMarkup projection, and the op engine: text edits as tracked
  changes, formatting, lists, tables, comments, review (accept and reject), reads and lint;
  a structural check that rolls back any transaction that makes the package less valid.
- `quillwork` (Python binding, PyO3, abi3).
- `quillwork-server`: document service, eight agent tools, MCP server and HTTP API. It depends
  on `quillwork~=0.0.2`.
- `@quillwork/viewer`: framework-free TypeScript viewer with a React wrapper (not published).
- `spec/`: op and view-model JSON Schemas, projection grammar, anchors, error codes and tool
  definitions.
- `tools/oracle`: Open XML validation, LibreOffice rendering and token checks.
- `bench/`: a reproducible fidelity and token benchmark on the public test documents.
- Runnable examples on a synthetic contract (`examples/`) and a Rust example
  (`crates/quillwork-core/examples/quickstart.rs`).
- Contributor docs, issue and pull request templates, CI on every pull request, and a publish
  workflow for PyPI and crates.io.
- The minimum supported Rust version is 1.88 (the `zip` dependency needs it).

[Unreleased]: https://github.com/gardan4/quillwork/compare/v0.0.4...HEAD
[0.0.4]: https://github.com/gardan4/quillwork/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/gardan4/quillwork/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/gardan4/quillwork/releases/tag/v0.0.2
