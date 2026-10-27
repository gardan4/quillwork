import type {
  Block,
  Cell,
  Image,
  ImageBlock,
  Inline,
  Marks,
  Paragraph,
  ParagraphFormat,
  Revision,
  Table,
  ViewModel,
  ViewerOptions,
} from "./types";
import { ATTR_SKIP, ATTR_TEXT } from "./text";

/** What block rendering needs from the viewer. */
export interface Ctx {
  doc: Document;
  vm: ViewModel;
  opts: ViewerOptions;
  /** Prefix for DOM ids, unique per viewer instance. */
  uid: string;
}

const EMU_PER_PX = 9525;

export function h<K extends keyof HTMLElementTagNameMap>(
  doc: Document,
  tag: K,
  cls?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const e = doc.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}

const pt = (twips: number) => `${+(twips / 20).toFixed(2)}pt`;

function luminance(hex: string): number {
  const n = parseInt(hex.slice(1), 16);
  const ch = (v: number) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * ch((n >> 16) & 255) + 0.7152 * ch((n >> 8) & 255) + 0.0722 * ch(n & 255);
}

/** Document colour as a private custom property, so the theme can adjust it in dark mode. */
export function setColor(e: HTMLElement, color: string) {
  e.style.setProperty("--_c", color);
  e.classList.add("qw-c");
  if (luminance(color) < 0.2) e.classList.add("qw-c-dk");
}

export function setShade(e: HTMLElement, color: string) {
  e.style.setProperty("--_bg", color);
  e.classList.add("qw-bg");
}

function fontStack(ctx: Ctx, font: string): string {
  const clean = (s: string) => `"${s.replace(/["\\]/g, "")}"`;
  const web = ctx.vm.meta.fonts?.[font];
  return [clean(font), web ? clean(web) : "", "var(--qw-doc-font)"].filter(Boolean).join(",");
}

/** Word highlight colour names (w:highlight). */
const HIGHLIGHT: Record<string, string> = {
  yellow: "#FFFF00",
  green: "#00FF00",
  cyan: "#00FFFF",
  magenta: "#FF00FF",
  blue: "#0000FF",
  red: "#FF0000",
  darkBlue: "#000080",
  darkCyan: "#008080",
  darkGreen: "#008000",
  darkMagenta: "#800080",
  darkRed: "#800000",
  darkYellow: "#808000",
  darkGray: "#808080",
  lightGray: "#C0C0C0",
  black: "#000000",
  white: "#FFFFFF",
};

function applyParagraphFormat(ctx: Ctx, e: HTMLElement, f: ParagraphFormat) {
  const s = e.style;
  if (f.align) {
    s.textAlign = f.align === "both" || f.align === "distribute" ? "justify" : f.align;
    if (f.align === "distribute") s.textAlignLast = "justify";
  }
  if (f.bidi) e.dir = "rtl";
  const ind = f.indent;
  if (ind) {
    if (ind.left) s.marginInlineStart = pt(ind.left);
    if (ind.right) s.marginInlineEnd = pt(ind.right);
    if (ind.hanging) s.textIndent = pt(-ind.hanging);
    else if (ind.firstLine) s.textIndent = pt(ind.firstLine);
  }
  const sp = f.spacing;
  if (sp) {
    if (sp.before != null) s.marginTop = pt(sp.before);
    if (sp.after != null) s.marginBottom = pt(sp.after);
    if (sp.line) {
      s.lineHeight = !sp.lineRule || sp.lineRule === "auto" ? String(+((sp.line / 240) * 1.15).toFixed(3)) : pt(sp.line);
    }
  }
  if (f.font) s.fontFamily = fontStack(ctx, f.font);
  if (f.size) s.fontSize = `${f.size / 2}pt`;
  if (f.color) setColor(e, f.color);
  if (f.shade) setShade(e, f.shade);
  if (f.b) s.fontWeight = "bold";
  if (f.i) s.fontStyle = "italic";
  if (f.caps) s.textTransform = "uppercase";
  if (f.pageBreakBefore) e.classList.add("qw-pbb");
}

const BORDER_STYLE: Record<string, string> = { double: "double", dotted: "dotted", dashed: "dashed", dashSmallGap: "dashed", dotDash: "dashed", dotDotDash: "dotted" };

/** Resolved cell borders; a dark line follows the theme's ink so it stays visible in dark mode. */
function applyBorders(td: HTMLElement, b: NonNullable<Cell["borders"]>) {
  for (const side of ["top", "right", "bottom", "left"] as const) {
    const l = b[side];
    const prop = `border-${side}`;
    if (!l || l.style === "none" || l.style === "nil") {
      td.style.setProperty(prop, "none");
      continue;
    }
    const color = !l.color || luminance(l.color) < 0.2 ? "var(--qw-ink)" : l.color;
    td.style.setProperty(prop, `${Math.max(l.size / 8, 0.5)}pt ${BORDER_STYLE[l.style] ?? "solid"} ${color}`);
  }
}

function applyMarks(ctx: Ctx, e: HTMLElement, m: Marks) {
  const s = e.style;
  if (m.b) s.fontWeight = "bold";
  if (m.i) s.fontStyle = "italic";
  const deco: string[] = [];
  if (m.u && m.u !== "none") {
    deco.push("underline");
    if (m.u === "double" || m.u === "dotted" || m.u === "dashed" || m.u === "wavy") s.textDecorationStyle = m.u;
    else if (m.u === "wave") s.textDecorationStyle = "wavy";
    else if (m.u.startsWith("dot")) s.textDecorationStyle = "dotted";
    else if (m.u.startsWith("dash")) s.textDecorationStyle = "dashed";
  }
  if (m.strike || m.dstrike) deco.push("line-through");
  if (m.dstrike) s.textDecorationStyle = "double";
  if (deco.length) s.textDecorationLine = deco.join(" ");
  if (m.sup || m.sub) {
    s.verticalAlign = m.sup ? "super" : "sub";
    s.fontSize = "smaller";
  }
  if (m.caps) s.textTransform = "uppercase";
  if (m.smallCaps) s.fontVariant = "small-caps";
  if (m.font) s.fontFamily = fontStack(ctx, m.font);
  if (m.size && !(m.sup || m.sub)) s.fontSize = `${m.size / 2}pt`;
  if (m.color) setColor(e, m.color);
  if (m.highlight && m.highlight !== "none") {
    const c = HIGHLIGHT[m.highlight] ?? (/^#[0-9A-Fa-f]{6}$/.test(m.highlight) ? m.highlight : null);
    if (c) {
      setShade(e, c);
      e.classList.add("qw-hlt");
    }
  }
  if (m.rtl) e.dir = "rtl";
  if (m.lang) e.lang = m.lang;
}

const AUTHOR_COLORS = 6;

function authorIndex(author: string): number {
  let x = 0;
  for (let i = 0; i < author.length; i++) x = (x * 31 + author.charCodeAt(i)) | 0;
  return Math.abs(x) % AUTHOR_COLORS;
}

const VERB: Record<string, string> = {
  ins: "Inserted",
  del: "Deleted",
  moveFrom: "Moved away",
  moveTo: "Moved here",
  fmt: "Formatted",
  pfmt: "Paragraph formatted",
  rowIns: "Row inserted",
  rowDel: "Row deleted",
  cellIns: "Cell inserted",
  cellDel: "Cell deleted",
};

export function formatDate(d?: string | null): string {
  if (!d) return "";
  const t = new Date(d);
  if (isNaN(t.getTime())) return d;
  return t.toISOString().slice(0, 16).replace("T", " ");
}

export function revisionLabel(r: Revision): string {
  let s = `${VERB[r.type] ?? r.type} by ${r.author}`;
  if (r.date) s += `, ${formatDate(r.date)}`;
  if (r.old) s += ` (${r.old})`;
  return s;
}

const isRemoval = (r: Revision) => r.type === "del" || r.type === "moveFrom" || r.type === "rowDel" || r.type === "cellDel";

/** Marks an element as a reviewable change: keyboard focusable, author colour, tooltip. */
export function markRevision(e: HTMLElement, r: Revision, kind: string) {
  e.classList.add("qw-rev", kind);
  e.dataset.qwRev = String(r.id);
  if (r.cs) e.dataset.qwCs = r.cs;
  e.dataset.qwAuthor = r.author;
  e.style.setProperty("--_rc", `var(--qw-author-${authorIndex(r.author)})`);
  e.tabIndex = 0;
  e.title = revisionLabel(r);
  e.setAttribute("aria-label", revisionLabel(r));
}

const FLAG_TITLE: Record<string, string> = {
  hidden: "Hidden text",
  white: "White text: invisible in Word",
  tiny: "Tiny text: barely visible in Word",
  zw: "Zero-width characters",
};

function safeUrl(u: string): string | null {
  return /^(https?:|mailto:)/i.test(u.trim()) ? u.trim() : null;
}

function renderInlineCore(ctx: Ctx, inl: Inline): HTMLElement {
  const doc = ctx.doc;
  switch (inl.k) {
    case "tab": {
      const e = h(doc, "span", "qw-tab", "\t");
      return e;
    }
    case "br": {
      if (inl.br === "line") {
        const e = h(doc, "br");
        e.setAttribute(ATTR_TEXT, "\n");
        return e as unknown as HTMLElement;
      }
      const e = h(doc, "span", inl.br === "page" ? "qw-pb" : "qw-colb");
      e.setAttribute("role", "separator");
      e.setAttribute("aria-label", inl.br === "page" ? "Page break" : "Column break");
      e.setAttribute(ATTR_TEXT, "\n");
      return e;
    }
    case "field": {
      const e = h(doc, "span", "qw-field", inl.t);
      e.dataset.qwField = inl.field.type;
      e.title = `Field: ${inl.field.instr}${inl.field.dirty ? " (needs update)" : ""}`;
      if (inl.marks) applyMarks(ctx, e, inl.marks);
      return e;
    }
    case "image": {
      const e = renderImage(ctx, inl.image, true);
      e.setAttribute(ATTR_SKIP, "");
      return e;
    }
    case "note": {
      const e = h(doc, "sup", "qw-noteref");
      e.setAttribute(ATTR_SKIP, "");
      const a = h(doc, "a", "", inl.label);
      a.href = `#${ctx.uid}-${inl.note}`;
      a.setAttribute("aria-label", `${inl.note.startsWith("fn") ? "Footnote" : "Endnote"} ${inl.label}`);
      e.append(a);
      return e;
    }
    default: {
      const e = h(doc, "span", "qw-t", inl.t);
      if (inl.marks) applyMarks(ctx, e, inl.marks);
      if (inl.style) e.dataset.qwStyle = inl.style;
      return e;
    }
  }
}

/** One inline with its common wrappers (flags, control, comments, link). Revision wrapping is the caller's. */
function renderInline(ctx: Ctx, inl: Inline): HTMLElement {
  let e = renderInlineCore(ctx, inl);
  if (inl.flags?.length) {
    for (const f of inl.flags) e.classList.add(`qw-f-${f}`);
    e.title = inl.flags.map((f) => FLAG_TITLE[f] ?? f).join("; ");
  }
  if (inl.control) {
    e.classList.add("qw-sdt");
    e.dataset.qwSdt = inl.control.type;
    const name = inl.control.alias || inl.control.tag;
    if (name) e.title = name + (e.title ? ` - ${e.title}` : "");
  }
  if (inl.comments?.length) {
    e.classList.add("qw-cm");
    e.dataset.qwC = inl.comments.join(" ");
    const all = inl.comments.map((id) => ctx.vm.comments.find((c) => c.id === id));
    if (all.every((c) => c?.done)) e.classList.add("qw-cm-done");
  }
  if (inl.fmtChange) markRevision(e, inl.fmtChange, "qw-fchg");
  if (inl.link) {
    const url = inl.link.url ? safeUrl(inl.link.url) : null;
    if (url) {
      const a = h(ctx.doc, "a", "qw-link");
      a.href = url;
      a.rel = "noopener noreferrer nofollow";
      a.target = "_blank";
      a.append(e);
      e = a;
    } else e.classList.add("qw-link");
  }
  return e;
}

export function renderParagraph(ctx: Ctx, p: Paragraph): HTMLElement {
  const doc = ctx.doc;
  const lvl = p.heading ?? 0;
  const e = doc.createElement(lvl >= 1 && lvl <= 6 ? `h${lvl}` : "p");
  e.className = "qw-p";
  if (lvl) {
    e.classList.add("qw-h", `qw-h${lvl}`);
    if (lvl > 6) {
      e.setAttribute("role", "heading");
      e.setAttribute("aria-level", String(lvl));
    }
  }
  e.dataset.qwId = p.id;
  e.dataset.qwKind = "p";
  e.dataset.qwStyle = p.style;
  if (p.fmt) applyParagraphFormat(ctx, e, p.fmt);
  if (p.control) {
    e.classList.add("qw-sdt-block");
    const name = p.control.alias || p.control.tag;
    if (name) e.title = name;
  }
  if (p.pprChange) {
    e.classList.add("qw-pchg");
    e.title = revisionLabel(p.pprChange);
  }
  if (p.list) {
    e.classList.add("qw-li");
    e.dataset.qwList = String(p.list.numId ?? "");
    e.setAttribute("aria-level", String(p.list.level + 1));
    const label = h(doc, "span", "qw-label", p.list.label);
    label.setAttribute(ATTR_SKIP, "");
    e.append(label);
  }
  // Consecutive inlines in the same revision share one <ins>/<del>: one focus stop per change.
  let revEl: HTMLElement | null = null;
  let revKey = "";
  for (const inl of p.inlines) {
    const child = renderInline(ctx, inl);
    const r = inl.rev;
    if (!r) {
      revEl = null;
      revKey = "";
      e.append(child);
      continue;
    }
    const key = `${r.id}:${r.type}`;
    if (!revEl || key !== revKey) {
      const removed = isRemoval(r);
      revEl = doc.createElement(removed ? "del" : "ins");
      markRevision(revEl, r, removed ? "qw-del" : "qw-ins");
      if (r.type === "moveFrom" || r.type === "moveTo") revEl.classList.add("qw-move");
      if (removed) revEl.setAttribute(ATTR_SKIP, "");
      if (r.date) revEl.setAttribute("datetime", r.date);
      revKey = key;
      e.append(revEl);
    }
    revEl.append(child);
  }
  if (p.mark) {
    const removed = isRemoval(p.mark);
    const m = h(doc, removed ? "del" : "ins", "qw-pmark", "\u00b6");
    markRevision(m, p.mark, removed ? "qw-del" : "qw-ins");
    m.setAttribute(ATTR_SKIP, "");
    e.classList.add(removed ? "qw-p-del" : "qw-p-ins");
    e.append(m);
  }
  if (!p.inlines.length) {
    const z = h(doc, "span", "qw-empty", "\u200b");
    z.setAttribute(ATTR_SKIP, "");
    e.append(z);
  }
  if (p.sectionEnd) e.classList.add("qw-sect-end");
  return e;
}

const DRAWING_LABEL: Record<string, string> = {
  picture: "Picture",
  chart: "Chart",
  smartart: "Diagram",
  shape: "Shape",
  ole: "Embedded object",
  watermark: "Watermark",
};

export function renderImage(ctx: Ctx, img: Image, inline: boolean): HTMLElement {
  const doc = ctx.doc;
  const box = h(doc, inline ? "span" : "figure", `qw-img qw-wrap-${img.wrap}`);
  box.dataset.qwId = img.id;
  box.dataset.qwKind = "image";
  box.dataset.qwImg = img.id;
  box.tabIndex = 0;
  const wpx = Math.max(1, Math.round(img.w / EMU_PER_PX));
  const hpx = Math.max(1, Math.round(img.h / EMU_PER_PX));
  // Reserve the box before the image loads: no layout jump.
  box.style.width = `${wpx}px`;
  box.style.aspectRatio = `${img.w} / ${img.h}`;
  if (img.align) box.classList.add(`qw-align-${img.align}`);
  if ((img.wrap === "square" || img.wrap === "tight") && (img.align === "left" || img.align === "right"))
    box.classList.add(`qw-float-${img.align}`);
  if (img.wrap === "behind" || img.wrap === "none") box.classList.add("qw-anchored");
  const label = img.decorative ? "" : img.alt || DRAWING_LABEL[img.drawing] || "Image";
  const tf: string[] = [];
  if (img.rotation) tf.push(`rotate(${+(img.rotation / 60000).toFixed(2)}deg)`);
  if (img.flipH || img.flipV) tf.push(`scale(${img.flipH ? -1 : 1},${img.flipV ? -1 : 1})`);

  if (img.asset && !img.external) {
    const asset = ctx.vm.assets[img.asset];
    const el = h(doc, "img");
    el.src = ctx.opts.assetUrl ? ctx.opts.assetUrl(img.asset, asset) : `/assets/${img.asset}`;
    el.alt = label;
    el.width = wpx;
    el.height = hpx;
    el.loading = "lazy";
    el.decoding = "async";
    el.draggable = false;
    if (img.title) el.title = img.title;
    if (img.crop) {
      const { l, t, r, b } = img.crop;
      const fw = Math.max(0.01, 1 - (l + r) / 100000);
      const fh = Math.max(0.01, 1 - (t + b) / 100000);
      el.classList.add("qw-crop");
      el.style.width = `${(100 / fw).toFixed(3)}%`;
      el.style.height = `${(100 / fh).toFixed(3)}%`;
      el.style.left = `${((-l / 100000 / fw) * 100).toFixed(3)}%`;
      el.style.top = `${((-t / 100000 / fh) * 100).toFixed(3)}%`;
    }
    if (tf.length) el.style.transform = tf.join(" ");
    box.append(el);
  } else {
    // External pictures are never fetched; shapes without a preview get a card.
    const ph = h(doc, "span", "qw-ph");
    ph.setAttribute("role", "img");
    ph.setAttribute("aria-label", label || DRAWING_LABEL[img.drawing] || "Image");
    let text = img.external ? "External image (not loaded)" : DRAWING_LABEL[img.drawing] ?? "Image";
    if (img.caption) text += `: ${img.caption}`;
    ph.textContent = text;
    box.append(ph);
  }
  if (!inline && img.caption && img.asset) {
    const cap = h(doc, "figcaption", "qw-sr", img.caption);
    box.append(cap);
  }
  if (label) box.setAttribute("aria-label", label);
  return box;
}

function renderImageBlock(ctx: Ctx, b: ImageBlock): HTMLElement {
  const e = renderImage(ctx, b, false);
  e.dataset.qwPara = b.para;
  if (b.rev) markRevision(e, b.rev, isRemoval(b.rev) ? "qw-del" : "qw-ins");
  return e;
}

export function renderTable(ctx: Ctx, t: Table): HTMLElement {
  const doc = ctx.doc;
  const table = h(doc, "table", "qw-table");
  table.dataset.qwId = t.id;
  table.dataset.qwKind = "table";
  if (t.style) table.dataset.qwStyle = t.style;
  if (t.bidi) table.dir = "rtl";
  if (t.caption) table.append(h(doc, "caption", "", t.caption));
  if (t.grid.length) {
    // Columns as shares of the grid, so a table narrower than its page width scales instead of overflowing.
    const total = t.grid.reduce((a, b) => a + b, 0) || 1;
    const cg = h(doc, "colgroup");
    for (const w of t.grid) {
      const col = h(doc, "col");
      col.style.width = `${+((w / total) * 100).toFixed(3)}%`;
      cg.append(col);
    }
    table.append(cg);
    table.style.width = pt(total);
  }
  // Grid column of each cell, for vMerge row spans.
  const starts = t.rows.map((row) => {
    let col = 0;
    return row.cells.map((c) => {
      const s = col;
      col += c.span || 1;
      return s;
    });
  });
  const continues = (r: number, col: number) => {
    const i = starts[r]?.indexOf(col) ?? -1;
    return i >= 0 && t.rows[r].cells[i].vmerge === "continue";
  };
  let head: HTMLTableSectionElement | null = null;
  let body: HTMLTableSectionElement | null = null;
  t.rows.forEach((row, r) => {
    const inHead = !!row.header && !body;
    const tr = h(doc, "tr", "qw-row");
    tr.dataset.qwId = row.id;
    tr.dataset.qwKind = "row";
    if (row.height) tr.style.height = pt(row.height);
    if (row.rev) markRevision(tr, row.rev, isRemoval(row.rev) ? "qw-row-del" : "qw-row-ins");
    row.cells.forEach((cell, i) => {
      if (cell.vmerge === "continue") return;
      const td = h(doc, inHead ? "th" : "td", "qw-cell");
      if (inHead) td.scope = "col";
      td.dataset.qwId = cell.id;
      td.dataset.qwKind = "cell";
      if (cell.span > 1) td.colSpan = cell.span;
      if (cell.vmerge === "restart") {
        let k = r + 1;
        while (k < t.rows.length && continues(k, starts[r][i])) k++;
        if (k - r > 1) td.rowSpan = k - r;
      }
      if (cell.width && !t.grid.length) td.style.width = pt(cell.width);
      if (cell.shade) setShade(td, cell.shade);
      if (cell.valign) td.style.verticalAlign = cell.valign === "center" ? "middle" : cell.valign;
      if (cell.borders) applyBorders(td, cell.borders);
      for (const b of cell.blocks) td.append(renderBlock(ctx, b));
      tr.append(td);
    });
    if (inHead) {
      if (!head) table.append((head = h(doc, "thead")));
      head.append(tr);
    } else {
      if (!body) table.append((body = h(doc, "tbody")));
      body.append(tr);
    }
  });
  return table;
}

export function renderBlock(ctx: Ctx, b: Block): HTMLElement {
  if (b.kind === "table") return renderTable(ctx, b);
  if (b.kind === "image") return renderImageBlock(ctx, b);
  return renderParagraph(ctx, b);
}

/** A block as a top-level node of a story: what patches replace by id. */
export function renderTopBlock(ctx: Ctx, b: Block, story: string): HTMLElement {
  const e = renderBlock(ctx, b);
  e.dataset.qwTop = "1";
  e.dataset.qwStory = story;
  return e;
}

export const sectionPt = pt;
