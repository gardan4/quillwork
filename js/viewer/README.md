# @quillwork/viewer

Renders a Quillwork view model (`spec/schema/view-model.schema.json`) as HTML and keeps it live by
applying patches (`spec/schema/patch.schema.json`). It shows tracked changes with accept/reject
controls, comments in a side rail, headers, footers and footnotes. Framework-free core (~16 KB gzip)
plus a React 19 wrapper.

```sh
npm install @quillwork/viewer
```

## Core API

```ts
import { render } from "@quillwork/viewer";

const view = render(container, viewModel, {
  assetUrl: (id, asset) => `/api/docs/42/assets/${id}`,  // default: /assets/{id}
  onReview: (changeId, action) => api.review(changeId, action), // "cs_7" | "r12", "accept" | "reject"
  onSelect: (sel) => chat.setContext(sel),                  // ViewerSelection | null
  theme: "auto",        // "light" | "dark" | "auto" (prefers-color-scheme)
  showHidden: true,     // hidden text dimmed and dotted
  rail: true,           // comment + change-set rail
  injectStyles: true,   // add the default stylesheet to <head> once
  flashMs: 1200,        // flash patched blocks; 0 = off
});
```

`render` returns a `QuillworkView`:

| Method | Does |
|---|---|
| `applyPatch(patch)` | Applies one patch. Returns `{ ok: true, version, changed }` or `{ ok: false, needsReload: true, reason, detail, version }`. `reason` is `"version_mismatch"` (patch base ≠ shown version), `"unknown_anchor"` or `"invalid_patch"`. On failure nothing is changed: fetch the full view model and call `setViewModel`. Only the blocks a patch names are re-rendered (keyed by anchor); they flash. |
| `setViewModel(vm)` | Full re-render. |
| `setOptions(partial)` | Change options without re-rendering the document. |
| `getSelection()` | `{ anchor, start, end, text, startBlock?, endBlock? }` for the current DOM selection inside the viewer, or `null`. `anchor` is a block anchor or a range `a..b`; offsets follow the text model below. |
| `highlight(target)` | `target` is an anchor, a range, or `{ anchor, start?, end? }`. An anchor or range outlines the blocks; `{ anchor, start, end }` on a paragraph marks exactly those characters via the CSS Custom Highlight API when the browser has it (otherwise the block). Returns false for an unknown anchor. |
| `clearHighlight()` | Removes it. |
| `scrollTo(anchor)` | Scrolls the block, row, cell, image or comment into view. |
| `find(anchor)` | The rendered element for an anchor. |
| `version`, `viewModel` | Shown state. |
| `layout()` | Re-positions rail cards (called automatically on resize). |
| `destroy()` | Removes listeners and DOM. |

Other exports: `injectStyles(document)`, `css` (the stylesheet string, for shadow roots or SSR), and
the text helpers `blockText`, `offsetOf`, `pointAt`, `ATTR_SKIP`, `ATTR_TEXT`. All view-model and
patch types are exported from `types.ts`.

## React

```tsx
import { QuillworkViewer } from "@quillwork/viewer/react";

<QuillworkViewer
  ref={viewRef}                 // the QuillworkView
  viewModel={vm}                // a new object = full reload
  patches={patches}             // append as they stream in; each is applied once, stale ones skipped
  onNeedsReload={() => refetch()}
  onReview={(id, action) => review(id, action)}
  onSelect={setSelection}
  theme="auto"
/>
```

Callbacks can change every render without re-rendering the document. `react` (^19) is an optional
peer dependency; the core does not import it.

## What is rendered

- Paragraphs with resolved run formatting, paragraph spacing/indent/alignment, list labels and
  headings (`h1`–`h6` from the outline level; deeper levels get `role="heading"` + `aria-level`).
- Tables with `gridSpan`, `vMerge` (rowspan), cell shading; column widths from the grid, scaled
  to fit.
- Images from `assetUrl`, inline and block, sized from EMU.
- Section page width and margins from `sections[]`; page and section breaks drawn as markers
  (no pagination).
- Header, footer and footnote/endnote stories; `PAGE`/`NUMPAGES` fields show their cached result.
- Tracked changes: insertions, deletions, moves and formatting changes, coloured per author, with a
  tooltip (author, date). Click or Enter on a change opens Accept/Reject; change sets get a card in the
  rail with Accept/Reject for the whole set.
- Comments as rail bubbles linked to their highlighted range; replies nested, resolved comments dimmed.

## Text model (selection offsets)

Offsets count a paragraph's accepted-mode text, the same text Quillwork's `find`/`expect` match:
text and field results count as shown, a tab is `"\t"`, a line/page/column break is `"\n"`;
deleted text, list labels, note marks, images and viewer chrome count nothing. Elements opt out with
`data-qw-x` or count as a literal string with `data-qw-t="…"`. A selection spanning paragraphs
returns a range anchor plus `startBlock`/`endBlock` so `start`/`end` are unambiguous.

## Theming

Every colour and size is a `--qw-*` custom property on `.qw-root`. `theme: "dark"` (or the
`.qw-dark` class) switches to the dark set; `"auto"` follows `prefers-color-scheme`.

| Variable | Use |
|---|---|
| `--qw-canvas`, `--qw-page`, `--qw-page-shadow` | Background around the page, the page, its shadow |
| `--qw-ink`, `--qw-ink-2`, `--qw-heading` | Body text, muted text, default heading colour |
| `--qw-rule`, `--qw-table-rule` | Hairlines, table grid |
| `--qw-accent`, `--qw-focus` | Buttons and focus rings |
| `--qw-ins`, `--qw-ins-bg`, `--qw-del`, `--qw-del-bg`, `--qw-fmt` | Tracked insert, delete, format change |
| `--qw-author-0` … `--qw-author-5` | Per-author revision colours |
| `--qw-comment`, `--qw-comment-bg`, `--qw-comment-active`, `--qw-bubble-bg` | Comment ranges and bubbles |
| `--qw-hl`, `--qw-flash`, `--qw-flash-ms` | `highlight()` colour, patch flash colour and duration |
| `--qw-warn`, `--qw-warn-bg` | Unsupported-content placeholders |
| `--qw-shadow`, `--qw-radius`, `--qw-gap`, `--qw-rail-width` | Rail cards and layout |
| `--qw-doc-font`, `--qw-ui-font`, `--qw-ui-size` | Fallback document font, UI font and size |

Below 760px container width the rail stacks under the page and the page drops its fixed width.

### Mapping your app's design tokens

```css
.qw-root,
.qw-root.qw-dark {
  --qw-canvas: var(--app-ground);
  --qw-page: var(--app-surface);
  --qw-ink: var(--app-ink);
  --qw-ink-2: var(--app-ink-2);
  --qw-rule: var(--app-rule);
  --qw-table-rule: var(--app-rule);
  --qw-accent: var(--app-accent);
  --qw-focus: var(--app-focus);
  --qw-bubble-bg: var(--app-surface);
}
```

This example assumes an app with its own `--app-*` tokens that it switches for dark mode
itself, so it passes `theme="light"` and lets the tokens carry the scheme (the revision colours
keep the viewer defaults unless also mapped).

## Accessibility

The document is `role="document"`; pages are `section`s with `header`/`footer`, the rail is an
`aside` with comments as `article`s, tables use real `table`/`th`, headings `h1`–`h6`. Lists are
exposed with `role="list"`/`listitem` via `aria-owns`, since list paragraphs stay flat siblings.
Every tracked change and comment is in the tab order; Enter or ArrowDown opens the review popover,
Escape returns focus. Labels in the rail are buttons. Reduced motion turns off the flash and
smooth scrolling; print hides viewer chrome.

## Development

```sh
npm install
npm run typecheck   # tsc --noEmit
npm test            # vitest + jsdom; validates spec/examples against the JSON Schemas with ajv
npm run build       # dist/index.js, dist/react.js + .d.ts
npm run size        # gzip sizes; fails above 30 KB for the core
npm run demo        # Vite demo on spec/examples: apply patches, stale patch, themes, highlight
```

The emitted `.d.ts` files use extensionless relative imports: consumers need
`moduleResolution: "bundler"` (or `node16` via the package `exports`).
