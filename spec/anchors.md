# Anchors

An anchor is a short, stable address for one thing in a document: a paragraph, a table, a row, a
cell, a drawing or a comment. Agents read anchors in the projection ([projection.md](projection.md))
and pass them back in ops ([schema/ops.schema.json](schema/ops.schema.json)). This file is the
normative grammar. The regular expressions are the ones in
[schema/common.schema.json](schema/common.schema.json); the tests check
[examples/anchors.json](examples/anchors.json) against them.

## Kinds

| Kind | Form | Example | Addresses |
|---|---|---|---|
| Body paragraph | `b` + serial | `b1dk` | a paragraph in the main body, a table cell included |
| Story paragraph | story + `-` + serial | `h1-02`, `fn3-1`, `c5-0` | a paragraph in a header, footer, footnote, endnote, comment or text box |
| Table | `t<n>` | `t2` | a table (nested tables get their own `t<n>`) |
| Row | `t<n>.r<i>` | `t2.r3` | the i-th row, 1-based |
| Cell | `t<n>.r<i>.c<j>` | `t2.r3.c1` | the j-th `w:tc` of that row, 1-based |
| Drawing | `img<n>` | `img4` | a picture, chart, diagram, shape, OLE object or watermark |
| Comment | `c<n>` | `c5` | a comment thread entry (its paragraphs are `c5-0`, `c5-1`, …) |

Story ids, used in `stories` keys of the view model and in patches:

| Story | Id | Story | Id |
|---|---|---|---|
| main body | `b` | footnote n | `fn<n>` |
| header n | `h<n>` | endnote n | `en<n>` |
| footer n | `f<n>` | comment n | `c<n>` |
| text box n | `tb<n>` | | |

## Grammar (ABNF, RFC 5234)

```abnf
anchor         = block-anchor / table-anchor / row-anchor / cell-anchor / image-anchor / comment-id

block-anchor   = body-block / story-block
body-block     = "b" 3*b32                       ; at least 3 serial characters
story-block    = story-prefix pos-int "-" 1*b32
story-prefix   = "h" / "f" / "fn" / "en" / "c" / "tb"

table-anchor   = "t" pos-int
row-anchor     = table-anchor ".r" pos-int
cell-anchor    = row-anchor ".c" pos-int
image-anchor   = "img" pos-int
comment-id     = "c" pos-int

story-id       = "b" / (story-prefix pos-int)

range          = range-start ".." range-end       ; anchor-or-range in the schema
range-start    = block-anchor / table-anchor / row-anchor / cell-anchor / image-anchor
range-end      = block-anchor / table-anchor / image-anchor
anchor-or-range = anchor / range

selection      = sel-item *("," sel-item)         ; what read accepts
sel-item       = anchor / (range-start ".." [range-end])   ; "b1fs.." = one window from b1fs

pos-int        = %x31-39 *DIGIT                   ; 1, 2, … no leading zero
b32            = DIGIT / %x61-68 / %x6A-6B / %x6D-6E / %x70-74 / %x76-7A
                 ; Crockford base32, lowercase: 0-9 a-h j k m n p-t v-z (no i l o u)
```

Matching regular expressions (anchored, case-sensitive):

```text
BlockAnchor  ^(b[0-9a-hjkmnp-tv-z]{3,}|(h|f|fn|en|c|tb)[1-9][0-9]*-[0-9a-hjkmnp-tv-z]+)$
TableAnchor  ^t[1-9][0-9]*$
RowAnchor    ^t[1-9][0-9]*\.r[1-9][0-9]*$
CellAnchor   ^t[1-9][0-9]*\.r[1-9][0-9]*\.c[1-9][0-9]*$
ImageAnchor  ^img[1-9][0-9]*$
CommentId    ^c[1-9][0-9]*$
StoryId      ^(b|(h|f|fn|en|c|tb)[1-9][0-9]*)$
```

Related ids that are not anchors: change sets `cs_<base36>` (`cs_7`), revisions `r<n>` in review
targets and the integer `id` in the view model (`12`), sections `s<n>`, assets `a_<16 hex>`.

## Serials

- **Alphabet.** Crockford base32, lowercase, most significant digit first. It has no `i l o u`, so
  a serial can't be misread as `1`/`0` and doesn't spell words.
- **Width.** Every story kind has a minimum width; serials are zero-padded to it and grow one
  character when the counter passes the width's range. Old handles keep their length.

  | Story kind | Min width | First serial | Handles before growing |
  |---|---|---|---|
  | body | 3 | `100` (1024) | 31,744 |
  | header, footer | 2 | `00` | 1,024 |
  | footnote, endnote, comment, text box | 1 | `0` | 32 |

  The body counter starts at 1024 so every body handle has a non-zero first digit and three or
  more characters, which keeps `b100` visually apart from the short story handles.
- **Minting.** Handles are minted in document order on the first load, then from a per-story
  counter for new paragraphs. A serial is **never reused**, also not after a delete is accepted.
  Story numbers (`h1`, `fn3`), `t<n>`, `img<n>` and `c<n>` are document-wide counters from 1, in
  document order on first load, never reused.
- **Persistence.** The sidecar map stores handle → `w14:paraId` (or position + text fingerprint for
  a paragraph without one), `t<n>` → the paraId of the table's first cell paragraph, `img<n>` →
  `wp:docPr/@id` plus the paraId of its paragraph, `c<n>` → `w:comment/@w:id`. The map is versioned
  with the document, so anchors are the same across sessions and processes.

## Positional anchors

Rows and cells are **positional**: `t2.r3.c1` means "third row, first `w:tc`" in the version the
agent read. Inserting a row renumbers the rows below it. They are safe because every op carries
`base_version`: if the table's structure changed after `base_version`, an op on a row or cell of
that table fails with `STALE_ANCHOR` and the fresh table lines. Within one transaction, row and
cell anchors refer to the table **as of `base_version`**, not as changed by earlier ops in the same
transaction.

Merged cells: a horizontally merged cell (`gridSpan`) is one `c<j>`; a vertically merged
continuation cell (`vMerge` continue) keeps its own `c<j>` but holds no editable content.

## Which anchor an op accepts

| Parameter | Accepts |
|---|---|
| block positions (`anchor`, `after`, `before` of text ops) | block anchor, table anchor, image anchor; a cell anchor when the cell holds exactly one paragraph |
| text targets (`find` inside an `anchor`) | block anchor; cell anchor (one-paragraph cell) |
| ranges (`a..b`) | both ends in the same story, `a` not after `b`; the range covers every top-level block from `a` to `b` |
| `read` selection | any anchor, closed or open ranges, comma-separated |

An image anchor used as a block position means the paragraph that holds only that drawing. When
the drawing sits inline in text, `after`/`before` resolve to its paragraph.

## Validity over time

1. An anchor is valid at version `v` if the thing exists in `v` (in accepted **or** markup view: a
   paragraph whose insertion is still tracked exists; a paragraph whose deletion is tracked still
   exists until the deletion is accepted).
2. Ops quote `base_version`. If the target changed after it, the op needs `expect` or fails with
   `STALE_ANCHOR` ([errors.md](errors.md)). If the target no longer exists at all, it fails with
   `NOT_FOUND` (`what: "anchor"`).
3. After an outside edit (a re-uploaded file), anchors are re-identified by paraId first and then
   by content similarity plus position. Paragraphs that can't be matched get new serials; their old
   anchors return `NOT_FOUND`.

## Examples

```text
b1dk              body paragraph
b1000             body paragraph minted after the first 31,744
h1-02             third paragraph minted in header 1
fn3-1             second paragraph of footnote 3
c5-0              first paragraph of comment 5
t2 / t2.r1 / t2.r1.c3
img4
b1dk..b1fr        range
b1fs..            next window from b1fs (read only)
b1dk,t2,img4      selection
```

Invalid: `b1` and `b10` (too short), `b1di` (`i` is not base32), `B1DK` (upper case), `h0-00`
(story numbers start at 1), `h1` (a story id, not an anchor), `t2.r0` (1-based), `b1dk...b1fr`.
