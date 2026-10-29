# Review / comments / tables / validate: notes for the spec owners

Notes from building `edit::{review, comments, tables}` and `validate`. Each item is a proposed
schema or spec change, a decision taken where the spec was silent, or a known gap. Items
marked **Resolved** are kept for the record and point to where the spec now covers them.

## Proposed schema / spec changes

1. **Resolved.** **`edit_comment`** is now `Op_edit_comment` in `ops.schema.json`, with
   `comment` and `text` both required, and has an example in `spec/examples/ops/`. It replaces
   the comment body and keeps the author, date and threading.
2. **Resolved.** **`change_set` needs the sidecar.** `spec/errors.md` now states that the engine
   resolves a `change_set` to its revision ids from its session log, and that an id it never
   issued is missing. The edit functions still take the resolved ids as
   `change_set_revisions: Vec<u32>`.
3. **`list_changes.status`**: the document alone only knows `open` revisions. `accepted` /
   `rejected` return an empty list. History would need the change-set log.
4. **`accept`/`reject` result**: `EditOutcome.revisions` lists the ids *resolved*, not created.
   A move resolves as a pair (moveFrom + moveTo, and their range markers), so accepting one move
   id resolves two. Proposal: spec it in OpResult.
5. **Resolved in the envelope.** **Structured reasons on errors.** `error.schema.json` now has
   a `reason` field whose enum includes `merged_cells`, `direct_only`, `no_grid`,
   `nested_table`, `unaligned_cells`, `uneven_split` and `beta`. Internally,
   `Error::Edit { code, message }` still carries the reason as a message prefix;
   `split_reason` in `src/ops/mod.rs` moves it into the envelope's `reason` field.
6. **Guards are the engine's job.** The edit functions read only `guards.mode`. `expect` and
   `base_version` are left to the engine, as is the per-op `author` override precedence (the
   comment ops honour `args.author` over the session author).

## Changes outside my files

- `src/wml/doc.rs`: `WordDoc::add_part` (adds a new XML part plus its content type and
  relationship, with or without a kept package; `write_into` adds it for MemParts documents) and
  `PartKind::{CommentsExtended, CommentsIds, CommentsExtensible, People}`. `add_comment` needs
  these to create `comments.xml`, the three extension parts and `people.xml` when they are
  missing.
- `src/error.rs`: appended `Error::Edit { code, message }`.
- `src/lib.rs`: `pub mod edit;`.

## Review (accept / reject)

- `tblGridChange` is not in `wml::view::RevisionKind`. `review` lists it itself as kind
  `gridChange`. Accepting drops the change; rejecting restores the old grid.
- `cellMerge` and `numberingChange` (and `w:numPr/w:ins`) are resolved by dropping the marker on
  both accept and reject. On reject the old merge state or numbering is **not** restored: the
  marker carries no old value for the merge, and `numberingChange` has only a legacy text form.
- Tracked **columns** (`cellIns` / `cellDel`) are beta and need `confirm: true` in track mode,
  else `CONFIRM_REQUIRED` (reason `beta`). Rejecting a `cellIns` or accepting a `cellDel` removes
  the cell and shrinks `tblGrid` by its span. `tblW` is not rescaled.
- Accepting a deleted paragraph mark (or rejecting an inserted one) merges the paragraph with
  the next: it keeps its own `pPr` and takes the next paragraph's mark run properties and
  section break. With no paragraph after it, an empty body paragraph is dropped when the block
  before it is a paragraph or a wholly inserted table (`drop_trailing_empty`, e.g. the separator
  `insert_table` adds at the end of the body).
- Rejecting an inserted footnote or endnote reference leaves the note itself in
  `footnotes.xml`. Word does the same; the note is orphaned but harmless.
- **Resolved.** The in-crate `TextMode::AcceptAll` view (`wml::text`) kept an empty line for a
  paragraph whose mark is deleted, where accepting merges it. It now runs that paragraph on into
  the next (and reject-all does the same for an inserted mark), as `view::paragraph_texts` does.

## Comments

- `resolve_comment` writes `w15:done` in `commentsExtended.xml`, creating the part if needed.
- `delete_comment` deletes the replies with the thread and removes every range marker and
  reference.
- `add_comment` with `find` anchors on the matched text (via `occurrence`); without it, the whole
  paragraph. Comments are allowed in the body, footnotes and endnotes only; anchoring in
  headers or footers is `UNSUPPORTED`.
- A reply gets its own `commentRangeStart`/`End` + `commentReference`, duplicated next to the
  root's markers as Word writes them, so `reply_comment` touches the host paragraph(s).

## Tables

- `merge_cells` and `split_cell` are direct only (`UNSUPPORTED: direct_only` in track mode):
  Word cannot track them.
- `merge_cells`: rectangular ranges whose edges do not cut a merged cell. `split_cell`: an
  even column split of a horizontally merged cell, or an un-merge of a vertical merge.
  Arbitrary splits are `uneven_split`.
- `set_cell` replaces the cell's content: one paragraph per `\n`-separated line, the old
  paragraphs deleted (tracked in track mode, where the last old paragraph's mark is kept and
  takes the final line). A cell that holds a nested table is `nested_table`.
- New rows copy the reference row's `trPr`/`tcPr`; a row inserted inside a vertical merge
  continues it (`vMerge` continue, empty text). New table paragraphs copy the anchor
  paragraph's `pPr` minus `numPr`, `pageBreakBefore`, `framePr`, `keepNext`, `outlineLvl` and
  section breaks.
- `insert_table`: `tblW` 100% (`pct` 5000), grid columns from the section's text width (page
  width minus side margins, default 9026 twips). Table style: the document's most-used one, else
  `TableGrid` if defined, else explicit single borders. The schema's `style` is read as the
  *cell paragraph* style (`inherit` | `P` | `H1`..`H9` | `LI` | a style id or name); there is no
  table-style argument. Proposal: say so in the schema, or add `table_style`. An empty
  paragraph follows the table when nothing, another table or the body `sectPr` follows, so
  adjacent tables do not fuse.

## Validate

- Rules and codes are in `src/validate/mod.rs`. Severity calibration: zero errors on all 18
  fixtures, 2 synthetic documents and 6 private documents.
- Not checked yet: child order in `settings.xml` and `styles.xml`, and full XSD types
  (only `jc`, table `jc`, `vAlign`, `vMerge`, on/off toggles and `w15:done` are typed).
  The full rule list is in the module doc of `src/validate/mod.rs`.
  `rPr` child order is a warning: Word tolerates most orders there and real documents violate it.
- `w:t` inside `w:moveFrom` is valid (Word writes moved-away text that way). Only `w:del` requires
  `w:delText`.
- A main part whose content type is not a WordprocessingML main type is
  `package.word_document_missing`, matching the fixture's expect.json.

## Oracle issues found (tools/oracle, not mine)

- **Resolved.** `locality.DEFAULT_ALLOWED_PARTS` listed `[Content_Types].xml`, which `fnmatch`
  reads as a character class, so a content-types change was always a `part_changed` violation.
  It is now escaped as `[[]Content_Types].xml`.
- The text oracle does not model `w:cellIns` / `w:cellDel`: the reject view of an inserted
  column still shows an empty cell, and the accept view of a deleted column does too. It also
  cannot merge an inserted paragraph mark when the next block is a table. The views match once
  empty lines are dropped. The actual accept/reject round trips match exactly.
