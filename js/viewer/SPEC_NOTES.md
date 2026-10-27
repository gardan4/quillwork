# Spec notes from building the viewer

Places where `spec/schema/*` was ambiguous or awkward to render. The viewer's current behaviour
is listed with each one, so nothing diverges silently. Each item ends with a concrete proposed change and, since the projection/view-model pass, how it was resolved.

1. **`TextInline.k` is optional.** All other inlines require `k`, so a consumer has to treat
   "no `k`" as text. The viewer does that.
   *Proposal:* make `k` required on `TextInline` (`"required": ["k", "t"]`), or document in the
   schema that a missing `k` means `"text"`.
   *Resolved:* the schema now says a missing `k` means `"text"` (kept optional so older results stay valid); Quillwork always writes `k`, and the Rust reader defaults it.

2. **Images have two shapes.** `ImageInline` nests the image under `image`, while `ImageBlock`
   flattens `ImageBase` into the block. The viewer normalises both into one render path.
   *Proposal:* nest the image in both (`ImageBlock.image: Image`), or flatten both; either way,
   use one shape.
   *Resolved (kept):* both shapes stay and the schema descriptions of `ImageInline` and `ImageBlock` say why. The viewer's normalising render path is the intended consumer pattern.

3. **Comment anchors have no character offsets.** `Comment.anchor` gives only `{start, end}`
   block anchors and `text`. The exact range comes from the inline `comments[]` arrays, so a host
   that only reads `comments` cannot highlight the range.
   *Proposal:* add an optional `startOffset`/`endOffset` (accepted-mode text offsets) to
   `Comment.anchor`.
   *Resolved:* `Comment.anchor` has optional `startOffset`/`endOffset` (UTF-16, text model of projection.md §10).

4. **Which id `onReview` passes.** There are two kinds of review unit: change sets (`cs_*`) and
   single revisions (`RevisionRef`, `r<n>`). The spec does not name the id the host's
   accept/reject op takes. The viewer passes `cs_7` for a rail card and `r12` for a single change.
   *Proposal:* state in the ops spec that `accept`/`reject` take `ChangeSetId | RevisionRef`.
   *Resolved:* `accept`/`reject` take `ReviewTarget` (common.schema.json): `cs_*`, `r<n>`, `author:<name>` or `all`; the viewer's ids are valid targets as they are.

5. **Selection offsets are undefined.** No spec defines the text model that character offsets
   count in, so the viewer defines it (README, "Text model"): accepted-mode text, `\t` for a tab,
   `\n` for a break, and nothing for deletions, labels or note marks. A selection across blocks
   has a range anchor and two offsets in different paragraphs, so the viewer adds
   `startBlock`/`endBlock`.
   *Proposal:* add the text model to `anchors.md`. Also add a `Selection` def to
   `common.schema.json`: `{anchor: AnchorOrRange, start, end, text, startBlock?, endBlock?}`.
   *Resolved:* the text model is spec/projection.md §10 (UTF-16 code units); `TextSelection` is in common.schema.json with `startBlock`/`endBlock`.

6. **Headings carry no formatting.** Heading paragraphs usually come with `fmt` absent and rely on
   the `Heading1..9` style. The view model does not resolve that style, so the viewer uses its own
   default heading sizes, which won't match the document.
   *Proposal:* resolve style formatting into `Paragraph.fmt`/run marks for headings as for any
   other style. Alternatively, add a top-level `styles` map (`styleId → {fmt, marks}`) that blocks
   reference.
   *Resolved:* `Paragraph.fmt` is the paragraph's resolved style formatting (font, size, colour, spacing, keepNext, plus run defaults `b`/`i`/`caps`), headings included; the viewer applies `b`/`i`/`caps`.

7. **Tables have no borders, padding or widths beyond the grid.** `Table` has
   `id, kind, section, style, caption, grid, bidi, rows`, and `Cell` has no borders or margins.
   The viewer draws a hairline grid and default cell padding, so borderless layout tables look
   ruled.
   *Proposal:* add `Table.borders` (resolved `{top,bottom,left,right,insideH,insideV}` as
   `{style,size,color} | null`), `Table.cellMargin`, and optional per-cell `borders`.
   *Resolved:* every `Cell` carries resolved `borders` (`{top,right,bottom,left}`, each `{style,size,color?}` or null) from the table style, table and cell properties, so no `Table.borders`/`cellMargin` is needed. The viewer draws them (dark lines follow the theme ink).

8. **`StoryId` admits `c<n>` for upserts and moves**, but comment bodies travel only in
   `comments[]`. Nothing says whether an upsert into story `c1` edits that comment's body. The
   viewer accepts it and applies it to `comments[c1].blocks` (it fails with `unknown_anchor`
   when the comment does not exist).
   *Proposal:* say so in the patch spec ("story `c<n>` addresses `comments[].blocks`"), or use a
   `BlockStoryId` without `c` in `Upsert.story`/`Position.story` and require a whole-comment
   `comments[]` entry instead.
   *Resolved:* patch.schema.json says story `c<n>` in `Upsert.story`/`Position.story` addresses `comments[].blocks`. Quillwork's generator sends whole comments in `comments[]` instead.

9. **Patches cannot remove a story.** `stories` in a patch adds stories, but deleting a footnote
   (or a header, when a section changes) leaves a stale story behind. The viewer would need a full
   reload.
   *Proposal:* add `storyRemoves: StoryId[]` to the patch, applied before `stories`.
   *Resolved:* `Patch.storyRemoves: StoryKeyId[]`, applied after `removes` and before `moves`; the viewer and the Rust `Patch::apply` handle it.

10. **Text boxes have no position.** `tb<n>` stories arrive with no link from the body block or
    drawing that hosts them. The viewer lists them after the body, labelled as text boxes.
    *Proposal:* add an inline `{k: "textbox", story: "tb1", width, height}` (or a block) where the
    text box is anchored.
    *Resolved:* `ImageBase.textbox` names the `tb<n>` story a drawing hosts. The viewer still lists text boxes after the body; linking them from the drawing is a viewer follow-up.

11. **`meta.flags` lacks projection flags.** The view model may contain projected content (tiny or
    white text, hidden text shown) that a reviewer should be warned about, but `meta.flags` has
    only security findings. The viewer shows a chip only for the existing flags.
    *Proposal:* add `tiny_text` and `white_text` to the `meta.flags` enum (they are already
    `InlineCommon.flags` values), so hosts can warn without scanning every inline.
    *Resolved:* `tiny_text` and `white_text` are in the `meta.flags` enum and the viewer has chips for them.

12. **View-model `stories` excludes `c` but `StoryId` includes it.** This is consistent with
    comments living in `comments[]`, but the two definitions disagree.
    *Proposal:* define `StoryKeyId` = `^(h|f|fn|en|tb)[1-9][0-9]*$` once in `common.schema.json`
    and use it for both the view model's `stories` keys and item 8.
    *Resolved:* `StoryKeyId` is in common.schema.json; the view model's `stories` keys and the patch's `stories`/`storyRemoves` use it.

13. **A range cannot end on a cell or row.** A range may start on a row or cell, but its end must
    be a block, table or image. So a selection that starts and ends inside one table has to be
    reported as the whole table (`t1`) plus offsets that only apply within one paragraph. The
    viewer does that, and its `startBlock`/`endBlock` carry the precise paragraphs.
    *Proposal:* let `RangeAnchor` end on `CellAnchor`/`RowAnchor` within the same table.
    *Open:* unchanged for v0. The viewer's `startBlock`/`endBlock` already give the precise paragraphs, so this only matters for hosts that read `anchor` alone.

14. **The last section's `sectionEnd` paragraph is redundant.** The final body paragraph carries
    `sectionEnd` for the document's last `w:sectPr`, which is always there. The viewer hides the
    marker on the last body block.
    *Proposal:* set `sectionEnd` only on paragraphs that end a non-final section; the last section
    is implied by `sections[-1]`.
    *Resolved:* `sectionEnd` is set only on paragraphs that end a non-final section.

15. **A patch could not say where a new comment goes.** `comments` upserts by id and the viewer
    appended new ids, but `add_comment` can insert a comment before existing ones (comments are
    ordered by their position in the document), so a patched view model drifted from a fresh one.
    *Proposal:* an optional `commentOrder` list in the patch, applied after `commentRemoves`.
    *Resolved:* `commentOrder` is in patch.schema.json; the engine sends it only when appending
    would give the wrong order, and the viewer sorts `comments` by it.
