# Projection

The projection is the text an agent reads: one block per line, the anchor first, Markdown-flavoured
inline marks, CriticMarkup for tracked changes. It is generated, never parsed back: agents send
ops ([schema/ops.schema.json](schema/ops.schema.json)), not edited projection text. This file is
normative for every binding; [examples/](examples/) holds the reference renderings and the tests
check them line by line against the view model.

Reference renderings of one invented document, version 42:

- [examples/projection-accepted.txt](examples/projection-accepted.txt): `view: accepted`
- [examples/projection-markup.txt](examples/projection-markup.txt): `view: markup`
- [examples/view-model.json](examples/view-model.json): the same document as the viewer's JSON
- [examples/projection-prose.txt](examples/projection-prose.txt): a plain 2.6-page contract used for
  the token budget

## 1. Overall shape

```text
projection   = header-line LF outline-line LF window-line LF
               LF
               *(block-line LF)                  ; the body window
               *(LF story-line LF *(block-line LF))   ; headers, footers, footnotes, …
               [LF appendix]
```

Every line matches `ProjectionLine` (`^(§ |\[|  )`): it is a `§` meta line, a block line starting
with `[`, or an indented continuation (table row, comment entry, expanded cell). Lines end in LF.
Text is Unicode NFC. Nothing in a line is wrapped.

## 2. Meta lines

### Header

```text
§ doc "<title>" · v<version> · <n> pages (est.) · <lang> · tracked changes: on|off
  · <n> open comment[s] · <n> open revision[s] [· flags: <flag>[,<flag>…]] · view: accepted|markup
```

(One line in the output; wrapped here for reading.)

- `title`: core property `dc:title`, else the first heading's text, else the file name without
  extension. Quotes inside are escaped as `\"`.
- `pages (est.)`: from `docProps/app.xml` `<Pages>` when present, else characters ÷ 1,800 rounded up.
  It is an estimate; the engine does not lay out pages.
- `lang`: the default language of the `Normal` style (`w:lang/@w:val`), BCP 47.
- `open revisions`: the number of open revision ids (`r12` and `r13` count as two).
- `flags` (only when there are any): `hidden_text`, `tiny_text`, `white_text`, `foreign_deletions`,
  `macros`, `external_links`, `protection`, `signed`. These flag untrusted or unusual content
  ([§7](#7-untrusted-content)); the same list is `meta.flags` in the view model.
- `view`: the projection mode of this rendering.

### Outline

```text
§ outline: [<anchor>] <label> <text> · [<anchor>] … 
```

Every heading (outline level 0–8 from the style or `w:outlineLvl`) in document order, with its
numbering label when it has one. Headings deeper than level 2 are left out when the line would be
longer than 1,500 characters; then the line ends in `· …`. The outline is re-sent in an op result
only when this line changed (`OpResult.outline`, which holds the whole line, `§ outline: ` prefix
included; null otherwise).

### Window

```text
§ window <first>..<last> (blocks <i>–<j> of <n>)
```

`<n>` counts top-level body blocks (a table is one block). A window is cut at a block boundary,
never inside a block or a table. The next window is read with `range: "<next>.."`.

### Story

```text
§ <story-id> <kind> · <section-id> <slot>
```

`kind` is `header`, `footer`, `footnote`, `endnote`, `textbox`; `slot` is `default`, `first` or `even`
for headers and footers and is left out for the others. Story sections come after the body window,
headers and footers first, only in the first window or when the selection asks for them. Comment
stories are rendered in the appendix instead.

### Section break

```text
§ section <section-id> · <type> [· landscape] [· <n> columns]
```

Before the first block of every section after the first. `type` is `next page`, `continuous`,
`even page` or `odd page`.

## 3. Block lines

```text
block-line   = "[" anchor [SP kind] [SP label] "]" [SP text]
kind         = "H" 1*DIGIT          ; heading level 1–9 (Heading n or outline level n-1)
             / "TITLE" / "QUOTE" / "CAPTION" / "TOC"
             / "STYLE=" style-id   ; any other non-default paragraph style
label        = "#" list-label [SP "L" DIGIT]
```

- **Default kind.** A paragraph in the story's default style (`Normal`, or the table's default
  paragraph style) has no kind: `[b101] De ondergetekenden:`.
- **List items.** A numbered or bulleted paragraph shows its rendered label: `[b102 #1.]`,
  `[b10c #3.1]`. Bullets render as `#•` (or the bullet glyph); levels below the top add `L<n>`:
  `[b10d #a. L1]`. A list item is never given a separate `LI` kind; the `#` says it.
- **Numbered headings** carry both: `[b105 H2 #1] Functie en arbeidsduur`.
- **Empty paragraphs** render as `[b10a]` with no text. Runs of more than two empty paragraphs are
  folded into one line `[b10a..b10c] (3 empty)`.
- **Paragraph format** (alignment, indent, spacing) is not shown; read it with `get_block`.

### Tables

Compact form, used when every cell holds exactly one paragraph and no nested table:

```text
[t1 TABLE 2×3 "Salaris"]
  [t1.r1 hdr] Schaal | Trede | Bruto per maand
  [t1.r2] 6 | 4 | € 2.912,00
```

- `R×C` is rows × grid columns. The quoted title is `w:tblCaption`, left out when there is none.
- `hdr` marks a repeating header row (`w:tblHeader`).
- Cells are separated by ` | `; a literal `|` in a cell is escaped as `\|`. A horizontally merged
  cell shows once followed by `|>` per extra grid column; a vertical-merge continuation shows `^`.
- The cell's text is addressed by its cell anchor (`t1.r2.c3`); the cell paragraphs' own body
  anchors (`b109`…) are valid too but not shown in the compact form.

Expanded form, used when any cell has more than one paragraph, a list, an image or a nested table:

```text
[t2 TABLE 3×2]
  [t2.r1] Onderdeel | Toelichting
  [t2.r2]
    [b11a] Werktijden
    ||
    [b11b] Ma–do 8:00–17:00
    [b11c #•] Pauze 30 minuten
```

The row line has no text; each cell's blocks follow, indented four spaces, cells separated by a
`||` line. Only rows that need it are expanded.

### Images

A paragraph that holds only one drawing renders as an image line with the drawing's anchor:

```text
[img1 IMAGE inline 4.0×1.5cm right alt="Logo Lindehof Kinderopvang"]
[img2 IMAGE anchor 6.0×4.0cm wrap=square alt="Plattegrond locatie"]
[img3 CHART inline 12.0×6.0cm alt="Bezetting per maand"]
```

- Kind: `IMAGE` (picture), `CHART`, `DIAGRAM` (SmartArt), `SHAPE`, `OLE`, `WATERMARK`.
- `inline` or `anchor` (floating) with `wrap=square|tight|through|top-bottom|behind|front`.
- Size in cm to one decimal, after crop. Alignment `left|center|right` only when not left.
- `alt="…"` from `wp:docPr/@descr`; `decorative` when marked decorative; `alt?` when missing (also
  a `missing_alt` lint finding).

A drawing inside a line of text renders inline as `![alt](img4)`.

## 4. Inline text

| Content | Rendering |
|---|---|
| bold, italic, underline | `**x**`, `*x*`, `__x__` |
| strike, superscript, subscript | `~~x~~`, `^x^`, `~x~` |
| other direct run formatting (colour, font, size, highlight) | not shown |
| hyperlink | `[text](https://…)`, internal: `[text](#bookmark)` |
| footnote / endnote reference | `[^fn3]`, `[^en1]` |
| tab, line break | `⇥`, `↵` |
| page break, column break | `⟨page⟩`, `⟨col⟩` |
| non-breaking space, soft hyphen | shown as the character (U+00A0, U+00AD) |
| field | `⟪TYPE args → "result"⟫` |
| content control | `⟪CTL tag="x" → "value"⟫`, empty: `⟪CTL tag="x" empty⟫` |
| comment | `{>>c1<<}` after the end of the commented range |
| hidden / suspicious text | `⟦hidden⟧text⟦/⟧` ([§7](#7-untrusted-content)) |
| symbol (`w:sym`) | the Unicode character when mappable, else `⟨sym F0FC⟩` |

Rules:

- **Marks are relative.** A mark shows only where it differs from the paragraph's resolved style,
  so a bold heading style or a bold header row shows no `**`.
- **Fields** show the field type, its arguments (switches as written, collapsed whitespace) and the
  cached result: `⟪REF _Ref4711 \r \h → "1"⟫`, `⟪PAGE → "1"⟫`,
  `⟪DATE \@ "d MMMM yyyy" → "1 februari 2027"⟫`. Text inside a field result can't be edited with
  `replace_text` (`IN_FIELD`); TOC fields render as one `[b… TOC] ⟪TOC \o "1-3" → 14 entries⟫` line (`1 entry` for one).
- **Content controls** show the tag (or alias when there is no tag) and the current value. A
  checkbox renders `→ ☒` or `→ ☐`.
- **Escaping.** A literal character that would start markup is preceded by `\`: `\*`, `\_`, `\^`,
  `\~`, `\[`, `\{`, `\|` (in tables), `\⟪`, `\⟦`, `\⟨`, `\\`. Characters that can't start markup
  are not escaped. Text agents write in ops uses the same escapes for `*` and `\`.

## 5. Tracked changes

The notation is CriticMarkup plus a revision tag:

```text
{++inserted++}  {--deleted--}  {~~old~>new~~}  {==formatted==}  {>>c1<<}
⟨r12,13 AI⟩  ⟨r9 M. Jansen⟩  ⟨fmt +bold r14 M. Jansen⟩  ⟨r20 AI moved from b10c⟩
```

- **Grouping.** Adjacent insertions and deletions by the same author in the same change set form a
  group; one tag follows the group and lists every revision id in it, ascending, comma-separated:
  `{--40--}{++38++}⟨r12,13 AI⟩`. The `{~~old~>new~~}` substitution form is not emitted (two
  revision ids read better as a del/ins pair); it is reserved.
- **Author.** The engine's configured agent author renders as `AI`; anyone else by display name.
- **Paragraph marks.** An inserted or deleted paragraph mark renders as `¶` inside the markup at the
  end of the line: `…einde.{--¶--}⟨r30 M. Jansen⟩`. A whole inserted paragraph is
  `[b120] {++Nieuwe tekst.¶++}⟨r31 AI⟩`.
- **Formatting changes** wrap the text in `{==…==}` and tag it with the change:
  `{==38 uur==}⟨fmt +bold r14 M. Jansen⟩`.
- **Moves** render as a deletion at the source and an insertion at the destination, the insertion
  tagged `moved from <anchor>`.
- **Table structure.** An inserted or deleted row is tagged on the row line:
  `[t1.r3] {++7 | 1 | € 3.050,00++}⟨r40 AI⟩`.

### Accepted vs markup

| | `accepted` (default) | `markup` |
|---|---|---|
| Other authors' changes | applied silently: insertions as plain text, deletions gone | shown with markup and tag |
| The agent's own **open** change sets | shown with markup and tag | shown with markup and tag |
| Comments | `{>>c1<<}` | `{>>c1<<}` |
| Hidden / suspicious text | marked | marked |

So in [projection-accepted.txt](examples/projection-accepted.txt) b10f reads "…een digitale
loonstrook." while in [projection-markup.txt](examples/projection-markup.txt) it reads
"…een{++ digitale++}⟨r9 M. Jansen⟩ loonstrook.". b106 shows `{--40--}{++38++}⟨r12,13 AI⟩` in both,
because cs_7 is the agent's own open change set: the agent keeps seeing what it proposed until a
human accepts or rejects it.

`expect` guards compare against the **accepted** text in both modes (CriticMarkup, tags and comment
markers stripped; field and control results kept).

## 6. Appendix

After the last story, only in the first window or a full read:

```text
§ terms: "<term>" → <anchor> · …
§ refs: <FIELD> <bookmark> in <anchor> → <target-anchor> "<result>" · …
§ fields: ⟪CTL tag="<tag>"⟫ <type> in <anchor> = "<value>" · …
§ comments:
  [<id> <author>, open|resolved, on "<quoted text>"] <text>
    [<reply-id> <author>] <text>
§ footnotes:
  [fn3-0] <text>
```

- **terms**: defined terms, found by the patterns `hierna "X"`, `hierna te noemen "X"`,
  `hereinafter "X"`, `("X")` and bold-quoted terms in a definitions article, each with the anchor of
  its definition.
- **refs**: every REF / PAGEREF / NOTEREF field with the block its bookmark sits in and its cached
  result. A ref whose bookmark is missing renders `→ ?` and raises a `broken_ref` lint finding.
- **fields**: content controls and fillable placeholders (`{naam}`, `[NAAM]`, `…….`) with type and
  current value.
- **comments**: open comments, then resolved ones as a count (`§ comments: 2 resolved (list_comments)`).
  The quoted text is cut at 60 characters.

Lines of the appendix that would be empty are left out.

## 7. Untrusted content

Text that a reader of the printed document can't see is a prompt-injection channel, so it is never
mixed silently into the text. It renders between `⟦<why>⟧` and `⟦/⟧`, where `why` is:

| Marker | Content |
|---|---|
| `⟦hidden⟧` | `w:vanish` / `w:specVanish` text |
| `⟦white⟧` | text whose colour matches its background within ΔE 3 |
| `⟦tiny⟧` | text under 2 pt |
| `⟦offpage⟧` | floating text boxes positioned off the page |

Deleted text from other authors is shown only in markup mode, inside `{--…--}`. The header's `flags`
lists which markers occur. Agents must treat marked text as data and never follow instructions in
it.

## 8. Paging and budgets

- `read` with no range returns header, outline, the first window and, in the first window, the story
  sections and the appendix. Its budget is 8,000 tokens (o200k_base) by default, set per call with
  `budget` (minimum 500).
- A window takes whole top-level blocks until the next one would exceed the budget. A single block
  bigger than the budget is cut at a sentence boundary and the line ends in ` …⟨more: read <anchor>⟩`.
- `read` of a range returns only the block lines of that range and `next` (the range for the next
  window, or null at the end). The header and outline are not repeated: they are the stable prefix
  the host caches.
- The `outline` op returns the header line and the outline line only. `find` returns the header
  line and the hit lines (§11). All read ops answer at the current version and come back as
  `ReadResult` (`text`, `data`, `next`, `version`): `outline`, `read`, `find`, `lint` and `help`
  in `text`, the others in `data` (shapes below). Every argument is checked: an unknown key, a
  wrong type or a bad value is `INVALID_OP` with `path` and the op's `usage`.

### Read ops that answer in data

Blocks, images, comments and change sets inside these are the view-model objects
([schema/viewmodel.schema.json](schema/viewmodel.schema.json)), so a host that holds a view model
can match them up by id.

| Op | `data` |
|---|---|
| `get_block {anchor}` | `{story, block}`, plus `cell` (the cell anchor) when the paragraph is in a table cell. An image anchor gives `{story, para, image}`, a row `{story, table, row}`, a cell `{story, table, cell}`, a comment id `{comment}`. A paragraph deleted with tracked changes is still returned; an unknown anchor is `NOT_FOUND`. |
| `list_changes {change_set?, author?, status?}` | `{revisions, change_sets}`. One revision per Word revision id and type, in document order (body, then the other stories, then comments): `{id, type, author, agent, date, change_set, story, anchors, text, old?}`. `type` is `ins`, `del`, `moveFrom`, `moveTo`, `fmt` or the other mark types of the view model; `agent` is true when the author is the engine's agent; `change_set` is null for revisions the engine did not make; `anchors` are the blocks it touches; `text` is its text, cut at 200 characters; `old` is the previous formatting of a `fmt` revision. `change_sets` are this session's change sets as in the view model. The filters combine: `author` matches the revision or change-set author, and `"AI"` also matches the agent; `status` (`open`, `accepted`, `rejected`, `partial`) is the change set's status, and a revision with no change set counts as `open`; a `change_set` that is not in the log is `NOT_FOUND`. |
| `list_comments {open_only?}` | `{comments: [{id, author, done, on, anchor, text, replies: [{id, author, text}]}]}`. Top-level comments in document order; replies (comments whose parent is that comment) are nested under it in date order, ties by their order in the file. `on` is the commented text, `anchor` the block it starts in. `open_only` drops comments marked done. |
| `list_images {}` | `{images}`: the view-model image objects plus `story`, `para` (the paragraph holding the drawing), `rev` when the image is inside a tracked insertion or deletion, and `media: {bytes, mime, px}` (`px` is `[w, h]`, or null when the format has no readable size; `media` is null for a linked picture or a shape without one). |
| `get_image {image\|asset}` | `{image, asset, mime, bytes, px, needs_conversion, base64}`. Exactly one of `image` (img3) or `asset` (a_…); `image` is null in the answer when the call named the asset. `needs_conversion` is true for formats a browser can't show (EMF, WMF, TIFF). Over 20 MB is `LIMIT` `image_bytes`; a linked picture or a drawing with no picture (shape, chart, text box) is `UNSUPPORTED` `unsupported_target`. |
| `list_fields {}` | `{controls, placeholders}`. A control is `{anchor, tag, alias, type, value, locked, bound, items}`: `value` is the checkbox state, the shown value, or null for an empty control; `items` lists the choices of a drop-down or combo box. A placeholder is `{anchor, text}` for `{name}`, `[NAME]` and runs of five or more `.`, `…` or `_` in the text. |

### `help`

`help {}` lists every op's signature, one per line, under `§ read ops` and `§ edit ops`; an edit op
the engine does not run yet ends in ` (not implemented)`. `help {name}` returns that one line. An
unknown name is `INVALID_OP`.

### `lint`

`lint {}` checks the current version and prints one line per finding, in document order:

```text
§ lint: <severity> <rule> <anchor> — <message>
```

or `§ lint: none`. At most 200 lines; the rest are counted in a last `§ lint: … (+<n> more)`.
Rules:

| Rule | Severity | When |
|---|---|---|
| `broken_ref` | warning | a cross-reference whose bookmark is missing, or whose target paragraph is deleted with tracked changes |
| `leftover_placeholder` | warning, info for a blank (`.....`, `___`) | a placeholder as `list_fields` reports it |
| `heading_order` | warning | a body heading (outside tables) more than one level below the previous one |
| `double_space` | info | exactly two spaces between words in a paragraph's accepted text (longer runs line things up on purpose) |
| `empty_paragraph` | info | a paragraph an edit of this session left empty (not a table cell's paragraph, which is the cell's required content, and not one deleted with tracked changes) |
| `missing_alt` | warning | a picture (embedded or linked) with no alt text that is not marked decorative |
| `invalid_markup` | warning (validator error), info (validator warning) | `lint` only: a finding of the engine's package validator; the message starts with the validator's code, and the anchor is `-` when the finding has no block |

`numbering_gap`, `stale_article_number` and `inconsistent_term` are in the schema's rule list but
not checked yet. The op result's `lint` (§9) runs the same rules except `invalid_markup`, on the
paragraphs the transaction changed (a table, row or cell by its paragraphs, an image by its
paragraph), plus every reference the transaction broke elsewhere: when it removed, deleted or moved
a bookmarked paragraph, all references are checked.

- Target (DESIGN §1): ≤ 550 tokens per page of body text and ≤ 1.25× the plain text.
  `spec/tests` measures the reference renderings and prints the numbers; see
  [README.md](README.md#token-budget).
- **Anchor overhead.** Measured with tiktoken o200k_base as the share of the block lines' tokens
  that the anchor brackets add over the same lines with only the list labels. It depends on the
  paragraph length, not the notation:

  | Corpus | Pages | Tokens / page | Anchor share | Without `[` `]` |
  |---|---|---|---|---|
  | [projection-prose.txt](examples/projection-prose.txt) (contract prose) | 2.6 | 505 | 14.1% | 11.7% |
  | 6 private real-world documents | 64 | 670 | 24.6% (16–49% per file) | 22.0% |
  | Synthetic long-paragraph documents | | | 8.7% | 7.3% |

  The earlier "≤ 10%" target only holds for long-paragraph text: forms, tables and letters have
  many short lines, where a 2–3 token anchor is a large share of a 4-token line. Dropping the
  brackets saves 2.4–2.6 points and makes a block line ambiguous with text that starts with an
  anchor-like word, so the brackets stay. The anchor is what makes every op addressable; the
  budget is met by paging, not by a shorter anchor.
- **Estimating tokens.** Bindings without a tokenizer budget with `characters / 4.5 + 5 per
  line`. On the real documents above this is 0.99–1.18× the tiktoken count (1.05× overall); it
  over-estimates long English prose (~1.3×) and under-estimates tiny documents whose meta lines
  dominate (down to ~0.75×), which is harmless at that size.

## 9. Results and errors as text

The JSON results carry projection lines in `changed`, `current` and `text`. A host that gives the
agent text (the MCP server does) renders them like this.

Op result:

```text
§ v42 · cs_7 · 1 changed
[b106] Werknemer treedt op ⟪CTL tag="startdatum" → "1 maart 2027"⟫ in dienst als pedagogisch medewerker{>>c1<<} voor {--40--}{++38++}⟨r12,13 AI⟩ uur per week.
```

followed, when present, by:

```text
§ removed: b10a, b10b
§ outline: [b100] … (the new outline line, only when headings changed)
§ lint: warning numbering_gap b10c — article 3.2 follows 3.0
§ warning: <text>
```

The first line is `§ v<version> · <change-set or "direct"> · <n> changed`. A direct-mode
transaction renders `direct`; an accept or reject renders `review`.

The `changed` lines are block lines as `read` prints them: markup view when the transaction was in
review mode or the host reads markup, else the accepted view with the transaction's author as the
agent. A changed paragraph that `read` folds into a larger line (a table cell's paragraph, a table
of contents entry) is reported as that larger line. The op result's `patch` updates the view model:
it lists removals and change sets always, and is complete (upserts, stories, comments,
`commentOrder`) once the host has fetched a view model from that engine.

Error:

```text
§ error STALE_ANCHOR · v42 · ops[0] replace_text · b106 · retryable
§ b106 changed after base_version 40; current text below.
[b106] Werknemer treedt op ⟪CTL tag="startdatum" → "1 maart 2027"⟫ in dienst als pedagogisch medewerker{>>c1<<} voor {--40--}{++38++}⟨r12,13 AI⟩ uur per week.
```

The first line has the code, the current version, the failing op (when there is one), the anchor
(when there is one) and `retryable` or `final`. The second is the message. Then come the `current`
lines, or the candidates as `  <anchor> #<occurrence>: <context>`, or `§ usage: <signature>` for
`INVALID_OP`. See [errors.md](errors.md) for every code.

## 10. Text model

Character offsets (`TextSelection`, `Comment.anchor.startOffset`, `find` hits, the viewer's
selection) count **UTF-16 code units** in one paragraph's text, built from its inlines in order:

| Inline | Counts as |
|---|---|
| text | the text, accepted view (other authors' insertions included, deletions left out) |
| tab | `\t` |
| line, page or column break | `\n` |
| field, content control | its current result / value |
| image, footnote / endnote mark, comment marker, list label | nothing |

The projection's escapes, CriticMarkup and revision tags are not part of the text. `expect` guards
hash the same text, NFC-normalised. In `markup` view (`find` only) tracked deletions count too.
A table, row or cell has no text of its own: offsets always belong to a paragraph, which is why a
selection that spans paragraphs carries `startBlock` and `endBlock`.

## 11. Find

`find {query, regex?, case_sensitive?, whole_word?, story?, view?, limit?}` searches the text
model of every story (body, then headers, footers, notes, text boxes, comments) or one `story`.
`query` is literal unless `regex` is set (Rust `regex` syntax: no look-around or backreferences;
patterns over 1,000 characters are refused). Matching is case-insensitive by default. A match
never crosses a paragraph boundary.

The result has `hits` (the first `limit`, default 50, max 500, in document order), `total` (every
match) and `lines` (the projection lines of the blocks the returned hits are in, each once). A hit
is `{anchor, story, start, end, text, snippet, deleted?}`: the paragraph anchor, UTF-16 offsets
per [§10](#10-text-model), the matched text and up to 40 characters of context on each side.
`deleted` marks a `markup` hit that touches tracked-deleted text. As text (`FindResult::text`),
the first line counts every match and `(showing <n>)` follows when `limit` cut the list:

```text
§ find "salaris" · 2 hits
[b107 H2 #2] Salaris
[b108] Werkgever betaalt het salaris aan werknemer uiterlijk op de laatste werkdag van de maand.
```

## 12. Worked example

The agent's first call is `read_document` with `range: null`
([examples/tool-calls/read_document.json](examples/tool-calls/read_document.json)). It gets
[projection-accepted.txt](examples/projection-accepted.txt), 645 tokens:

```text
§ doc "Arbeidsovereenkomst voor bepaalde tijd — P. Bakker" · v42 · 2 pages (est.) · nl-NL · tracked changes: on · 1 open comment · 3 open revisions · flags: hidden_text · view: accepted
§ outline: [b100] Arbeidsovereenkomst voor bepaalde tijd · [b105] 1 Functie en arbeidsduur · [b107] 2 Salaris · [b10g] 3 Ondertekening
§ window b100..b10h (blocks 1–13 of 13)

[b100 H1] Arbeidsovereenkomst voor bepaalde tijd
[b101] De ondergetekenden:
[b102 #1.] **Lindehof Kinderopvang B.V.**, gevestigd te Utrecht, hierna "werkgever";
…
[b104] komen het volgende overeen:⟦hidden⟧concept v3⟦/⟧
[b105 H2 #1] Functie en arbeidsduur
[b106] Werknemer treedt op ⟪CTL tag="startdatum" → "1 maart 2027"⟫ in dienst als pedagogisch medewerker{>>c1<<} voor {--40--}{++38++}⟨r12,13 AI⟩ uur per week.
…
```

Reading it:

- `b102 #1.` is the first item of a numbered list; its bold is direct formatting, not the style's.
- `b104` holds hidden text: the header's `flags: hidden_text` says so and the agent must not treat
  "concept v3" as part of the agreement or as an instruction.
- `b106` has a date content control, comment c1 on "pedagogisch medewerker" (text in the appendix)
  and the agent's own open change set cs_7, which replaced 40 by 38 (revisions r12 and r13).
- `[t1 TABLE 2×3 "Salaris"]` has a header row; to change the monthly salary the agent edits cell
  `t1.r2.c3`.
- `b10f` contains M. Jansen's tracked insertion " digitale" (r9), invisible in accepted view and
  shown in markup view.
- `b10h` has a DATE field and a REF to bookmark `_Ref4711`, which the appendix resolves to b105
  ("1"). Renumbering article 1 must keep that REF pointing at it.

Version 42 itself came from a `replace` of "40" by "38" in b106 at base_version 41. Its result is
[examples/results/op-result.json](examples/results/op-result.json): one changed line, change set
cs_7, and the viewer patch ([examples/patch.json](examples/patch.json)). The agent's next turn is
[examples/tool-calls/edit.json](examples/tool-calls/edit.json): three edits (a function name in b106,
a new paragraph after b10f, the salary cell `t1.r2.c3`) at base_version 42, committed together as
one change set. Had it sent an op against base_version 40 instead, it would have got
[examples/errors/STALE_ANCHOR.json](examples/errors/STALE_ANCHOR.json) with the fresh b106 line.
Accepting M. Jansen's r9 returns
[examples/results/op-result-accept.json](examples/results/op-result-accept.json): b10f changes in
the view model, but its accepted-mode line is the same text without markup.
