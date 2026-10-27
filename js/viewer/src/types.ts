/**
 * Types for the Quillwork view model and view-model patch, hand-written from
 * spec/schema/view-model.schema.json, patch.schema.json and common.schema.json (spec 0.1.0).
 * Units stay in OOXML units: twips (1/20 pt), EMU (914400 per inch), half points.
 * test/schema.test.ts checks the spec examples against the schemas and against these types.
 */

// ---------- common.schema.json ----------

/** "b1dk", "h1-02", "fn3-1", "c5-0". */
export type BlockAnchor = string;
/** "t2" */
export type TableAnchor = string;
/** "t2.r1" */
export type RowAnchor = string;
/** "t2.r1.c3" */
export type CellAnchor = string;
/** "img4" */
export type ImageAnchor = string;
/** "c5" */
export type CommentId = string;
/** "s1" */
export type SectionId = string;
/** "b" | "h1" | "f1" | "fn1" | "en1" | "c1" | "tb1" */
export type StoryId = string;
/** A story other than the body or a comment: "h1" | "f1" | "fn1" | "en1" | "tb1" */
export type StoryKeyId = string;
/** "cs_7" */
export type ChangeSetId = string;
/** "a_3c9e1f0a7b2d4e65" */
export type AssetId = string;
/** Any addressable thing: block, table, row, cell, image or comment. */
export type Anchor = string;
/** One anchor, or an inclusive range "a..b" of top-level blocks in one story. */
export type AnchorOrRange = string;
/** "#RRGGBB", upper case. */
export type Color = string;
/** 1/20 pt. */
export type Twips = number;
/** 914400 per inch. */
export type Emu = number;
/** 22 = 11 pt. */
export type HalfPoints = number;
/** RFC 3339 date-time. */
export type DateTime = string;
export type RevisionId = number;
export type Version = number;

// ---------- view-model.schema.json ----------

export type DocFlag =
  | "macros"
  | "external_links"
  | "active_content"
  | "suspicious_vba"
  | "embedded_fonts"
  | "hidden_text"
  | "tiny_text"
  | "white_text";

export interface Meta {
  title: string | null;
  lang: string;
  trackChanges: boolean;
  labels: string[];
  pagesEstimate?: number;
  readOnly?: "signed" | "strict" | "protected" | null;
  flags: DocFlag[];
  /** Document font name to the web font the viewer uses (Calibri → Carlito). */
  fonts?: Record<string, string>;
}

export interface HeaderFooterRefs {
  default: string | null;
  first: string | null;
  even: string | null;
}

export interface Section {
  id: SectionId;
  page: {
    w: Twips;
    h: Twips;
    orient?: "portrait" | "landscape";
    margins: {
      top: number;
      right: number;
      bottom: number;
      left: number;
      header?: number;
      footer?: number;
      gutter?: number;
    };
    cols: number;
  };
  headers: HeaderFooterRefs;
  footers: HeaderFooterRefs;
  titlePage: boolean;
  breakType?: "nextPage" | "continuous" | "evenPage" | "oddPage" | "nextColumn";
}

export type StoryKind = "header" | "footer" | "footnote" | "endnote" | "textbox";

export interface Story {
  kind: StoryKind;
  blocks: Block[];
}

export type RevisionType =
  | "ins"
  | "del"
  | "fmt"
  | "pfmt"
  | "moveFrom"
  | "moveTo"
  | "rowIns"
  | "rowDel"
  | "cellIns"
  | "cellDel";

export interface Revision {
  id: RevisionId;
  type: RevisionType;
  author: string;
  date?: DateTime;
  cs: ChangeSetId | null;
  /** For fmt/pfmt: what changed, in projection notation. */
  old?: string;
}

export interface ParagraphFormat {
  align?: "left" | "center" | "right" | "both" | "distribute";
  bidi?: boolean;
  indent?: { left?: Twips; right?: Twips; firstLine?: Twips; hanging?: Twips };
  spacing?: { before?: Twips; after?: Twips; line?: number; lineRule?: "auto" | "exact" | "atLeast" };
  font?: string;
  size?: HalfPoints;
  color?: Color;
  shade?: Color;
  /** Paragraph-level run defaults (from the style), resolved. */
  b?: boolean;
  i?: boolean;
  caps?: boolean;
  keepNext?: boolean;
  pageBreakBefore?: boolean;
}

export interface Marks {
  b?: boolean;
  i?: boolean;
  /** Underline style: single, double, dotted, wave, none, ... */
  u?: string;
  strike?: boolean;
  dstrike?: boolean;
  sup?: boolean;
  sub?: boolean;
  caps?: boolean;
  smallCaps?: boolean;
  font?: string;
  size?: HalfPoints;
  color?: Color;
  /** Word highlight colour name (yellow, green, darkBlue, ...). */
  highlight?: string;
  rtl?: boolean;
  lang?: string;
}

export type ContentControlType =
  | "plainText"
  | "richText"
  | "date"
  | "dropdown"
  | "comboBox"
  | "checkbox"
  | "picture"
  | "group"
  | "other";

export interface ContentControl {
  tag: string | null;
  alias: string | null;
  type: ContentControlType;
  bound?: boolean;
  locked?: boolean;
}

export type InlineFlag = "hidden" | "white" | "tiny" | "zw";

export interface InlineCommon {
  rev?: Revision | null;
  fmtChange?: Revision | null;
  comments?: CommentId[];
  link?: { url?: string; bookmark?: string } | null;
  flags?: InlineFlag[];
  control?: ContentControl | null;
}

/** `k` may be omitted on input (an inline without `k` is text); Quillwork always writes it. */
export interface TextInline extends InlineCommon {
  k?: "text";
  t: string;
  marks?: Marks;
  style?: string;
}

export interface TabInline extends InlineCommon {
  k: "tab";
}

export interface BreakInline extends InlineCommon {
  k: "br";
  br: "line" | "page" | "column";
}

export interface FieldInline extends InlineCommon {
  k: "field";
  /** Cached result as Word displays it. */
  t: string;
  marks?: Marks;
  field: { type: string; instr: string; editable: boolean; dirty?: boolean };
}

export interface ImageInline extends InlineCommon {
  k: "image";
  image: Image;
}

export interface NoteRefInline extends InlineCommon {
  k: "note";
  /** fn<n> or en<n>. */
  note: string;
  label: string;
}

export type Inline = TextInline | TabInline | BreakInline | FieldInline | ImageInline | NoteRefInline;

export interface Paragraph {
  id: BlockAnchor;
  kind: "p";
  section?: SectionId;
  style: string;
  styleName?: string;
  heading?: number | null;
  list?: { label: string; level: number; numId?: number } | null;
  fmt?: ParagraphFormat;
  inlines: Inline[];
  mark?: Revision | null;
  pprChange?: Revision | null;
  control?: ContentControl | null;
  sectionEnd?: boolean;
}

export type DrawingKind = "picture" | "chart" | "smartart" | "shape" | "ole" | "watermark";
export type Wrap = "inline" | "square" | "tight" | "topAndBottom" | "none" | "behind" | "through";

export interface Image {
  id: ImageAnchor;
  drawing: DrawingKind;
  /** Null for an external (never fetched) picture or a shape without a preview. */
  asset: AssetId | null;
  external?: boolean;
  w: Emu;
  h: Emu;
  wrap: Wrap;
  align?: "left" | "center" | "right" | null;
  alt: string | null;
  title?: string | null;
  decorative: boolean;
  /** a:srcRect in 1/1000 percent. */
  crop: { l: number; t: number; r: number; b: number } | null;
  /** 60 000ths of a degree. */
  rotation: number;
  flipH?: boolean;
  flipV?: boolean;
  caption?: string | null;
  /** Story key of the text box this drawing holds (`stories[textbox]`). */
  textbox?: StoryKeyId;
}

export interface ImageBlock extends Image {
  kind: "image";
  para: BlockAnchor;
  section?: SectionId;
  rev?: Revision | null;
}

export interface Cell {
  id: CellAnchor;
  span: number;
  vmerge: "restart" | "continue" | null;
  width?: Twips | null;
  shade?: Color | null;
  valign?: "top" | "center" | "bottom";
  /** Resolved borders (table style, table and cell properties); null = no line. */
  borders?: { top: BorderLine | null; right: BorderLine | null; bottom: BorderLine | null; left: BorderLine | null };
  blocks: Block[];
}

export interface BorderLine {
  /** single, double, dotted, dashed, ... */
  style: string;
  /** Eighths of a point. */
  size: number;
  color?: Color;
}

export interface Row {
  id: RowAnchor;
  header?: boolean;
  height?: Twips | null;
  rev?: Revision | null;
  cells: Cell[];
}

export interface Table {
  id: TableAnchor;
  kind: "table";
  section?: SectionId;
  style?: string | null;
  caption?: string | null;
  grid: Twips[];
  bidi?: boolean;
  rows: Row[];
}

export type Block = Paragraph | Table | ImageBlock;

export interface Comment {
  id: CommentId;
  parent: CommentId | null;
  author: string;
  initials?: string | null;
  date: DateTime | null;
  done: boolean;
  agent?: boolean;
  /** startOffset/endOffset: UTF-16 offsets of the range ends in their paragraphs' text. */
  anchor: { start: Anchor; end: Anchor; text: string; startOffset?: number; endOffset?: number } | null;
  blocks: Block[];
}

export interface ChangeSet {
  id: ChangeSetId;
  label: string | null;
  author: string;
  turn: string | null;
  version?: Version;
  revisions: RevisionId[];
  untracked?: string[];
  status: "open" | "accepted" | "rejected" | "partial";
}

export interface Asset {
  mime: "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/svg+xml";
  source?: string;
  /** Natural size in pixels [w, h]. */
  px: [number, number];
  bytes: number;
}

export interface ViewModel {
  spec_version: string;
  version: Version;
  meta: Meta;
  sections: Section[];
  /** The body story (b), in document order. */
  blocks: Block[];
  stories: Record<StoryId, Story>;
  comments: Comment[];
  changeSets: ChangeSet[];
  assets: Record<AssetId, Asset>;
}

// ---------- patch.schema.json ----------

export interface Position {
  id: string;
  story: StoryId;
  /** Sibling block id it follows; null = first in its story. */
  after: string | null;
}

export interface Upsert {
  story: StoryId;
  /** Where a new block goes; ignored when the id already exists. */
  after: string | null;
  block: Block;
}

export interface Patch {
  spec_version: string;
  base_version: Version;
  version: Version;
  removes: string[];
  moves: Position[];
  upserts: Upsert[];
  /** Stories deleted whole (a removed header, footnote, text box). */
  storyRemoves?: StoryKeyId[];
  /** Stories replaced whole. */
  stories?: Record<StoryKeyId, Story>;
  comments?: Comment[];
  commentRemoves?: CommentId[];
  /** Every comment id in its new order, when appending new comments would leave another order. */
  commentOrder?: CommentId[];
  changeSets?: ChangeSet[];
  meta?: Meta;
  sections?: Section[];
  assets?: Record<AssetId, Asset>;
}

// ---------- viewer API ----------

/**
 * A selection or pointer in the document, per spec/anchors.md.
 * `anchor` is one block/image anchor or a range "a..b" of top-level blocks in one story.
 * `start`/`end` are offsets in the accepted-mode text of the first and last paragraph
 * (see README "Text model"). `text` is the selected accepted-mode text.
 */
export interface ViewerSelection {
  anchor: AnchorOrRange;
  start: number;
  end: number;
  text: string;
  /** Paragraph anchors the offsets count in (equal to `anchor` for a one-block selection). */
  startBlock?: BlockAnchor;
  endBlock?: BlockAnchor;
}

/** What `highlight()` accepts: an anchor, a range "a..b", or a character range in one block. */
export type HighlightTarget = AnchorOrRange | { anchor: Anchor; start?: number; end?: number };

export type ReviewAction = "accept" | "reject";

export type PatchResult =
  | { ok: true; version: Version; changed: string[] }
  | {
      ok: false;
      needsReload: true;
      reason: "version_mismatch" | "unknown_anchor" | "invalid_patch";
      detail: string;
      /** The version the viewer is at. */
      version: Version;
    };

export interface ViewerOptions {
  /** URL for an asset id. Default: `/assets/{id}`. */
  assetUrl?: (assetId: AssetId, asset: Asset | undefined) => string;
  /**
   * Accept or reject a change. `changeId` is a change-set id ("cs_7") or a revision ref
   * ("r12", the spec's RevisionRef).
   */
  onReview?: (changeId: string, action: ReviewAction) => void;
  /** Called when the selection or the clicked image changes; null when nothing is selected. */
  onSelect?: (selection: ViewerSelection | null) => void;
  /** "auto" follows prefers-color-scheme. Default "auto". */
  theme?: "light" | "dark" | "auto";
  /** Show hidden text (dimmed, dotted). Default true. */
  showHidden?: boolean;
  /** Show the comment and change rail. Default true. */
  rail?: boolean;
  /** Inject the default stylesheet into the document head. Default true. */
  injectStyles?: boolean;
  /** Flash duration for patched blocks, ms. 0 turns it off. Default 1200. */
  flashMs?: number;
}
