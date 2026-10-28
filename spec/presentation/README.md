# Presentation candidate editing

Contract version: **0.1.0**. This is a separate API from the Word operation catalogue and viewer schema. Rust exposes `pml::Presentation`; Python exposes `quillwork.Presentation`. The reference HTTP/MCP service still handles Word documents.

The engine opens transitional `.pptx` packages and edits existing DrawingML paragraphs in shapes, groups, table cells and speaker notes. It preserves the original package and unrelated XML. Layouts, masters, themes, animations, charts and assets are preserved without becoming editable features.

## Identity and guards

Anchors use the native slide ID and shape ID: `s256.sh2.p1`. Notes use `s256.n.sh2.p1`; table paragraphs use `s256.sh4.r2.c1.p1`. Slide/shape IDs survive text edits and reordering. Paragraph, row and cell indices are positional, so every request based on persisted bytes should also carry `base_sha256` from `describe()`.

Versions start at 1 on each open and increment once per successful transaction. An exact `base_version` protects a live object; the SHA256 digest protects against external edits or reopening a different candidate at version 1. Neither is a distributed writer lock: the host must atomically compare its stored source version/digest when committing candidate bytes.

## Transactions

[transaction.schema.json](transaction.schema.json) defines the JSON request. A transaction has 1–200 operations, with at most 1,000,000 combined UTF-8 bytes across `find` and `replace`. Unknown fields are refused. If supplied, `spec_version` must equal `0.1.0`. Any failed operation leaves bytes, version and review state unchanged.

```json
{
  "spec_version": "0.1.0",
  "base_version": 1,
  "ops": [{
    "op": "replace_text",
    "anchor": "s256.sh2.p1",
    "find": "Old client",
    "replace": "New client",
    "expect": "Proposal for Old client"
  }]
}
```

`find` matches literal text across existing runs. It must match exactly once unless `occurrence` selects a one-based occurrence. `expect` guards the complete paragraph. Inserted text inherits the first changed run's formatting. Unchanged runs and text outside the replacement retain their formatting. Inserting paragraph breaks/tabs, changing formatting, and editing empty paragraphs or paragraphs containing fields/breaks are outside this initial contract.

Python `fill(values, expected_counts=...)` compiles literal `{{name}}` slots into this same transaction. Slot names match `[A-Za-z_][A-Za-z0-9_.-]*`. Slots can span runs. Values are plain text, never template code and never recursively expanded. Every provided slot must occur; every discovered slot needs a value. Optional expected counts must match every supplied slot exactly. Counts and unsupported-target checks complete before mutation.

## Reads and review

`read()` returns a compact anchored projection. `describe()` returns the format, capabilities, current/source hashes, slide order, shapes, paragraphs, visibility hints and local geometry. Layout/master formatting is preserved but is not fully resolved. Unsupported shapes and inherited text may be absent from the projection; this is not a complete visual transcription or a layout engine.

`review()` returns cumulative changed paragraph text with original/candidate SHA256 digests. The report describes a candidate and does not accept or reject it. PowerPoint edits are not Word tracked changes. Hosts retain original and candidate bytes, render affected slides, authorize the reviewer, and bind approval to the exact candidate digest. Rejection retains the original package; selective historical undo is not provided.

Signed, write-protected, encrypted and macro-enabled presentations are refused. External relationships are reported as data and never fetched. Other unknown content remains in the package and can execute if opened by an application that supports it; hosts remain responsible for active-content policy and safe rendering.
