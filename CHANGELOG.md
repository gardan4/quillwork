# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Version numbers follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html), with the usual 0.x caveat: until 1.0,
any release may change the op language, the projection or the sidecar format, and such changes
are listed under **Changed**.

## [Unreleased]

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

[Unreleased]: https://github.com/gardan4/quillwork/compare/v0.0.2...HEAD
[0.0.2]: https://github.com/gardan4/quillwork/releases/tag/v0.0.2
