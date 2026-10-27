import type {
  Block,
  ChangeSet,
  Comment,
  HighlightTarget,
  Patch,
  PatchResult,
  Section,
  Story,
  ViewerOptions,
  ViewerSelection,
  ViewModel,
} from "./types";
import { blockText, offsetOf, pointAt } from "./text";
import { type Ctx, formatDate, h, renderBlock, renderTopBlock, sectionPt } from "./render";
import { css } from "./styles";

let instances = 0;

/** A4 portrait with 1 inch margins, for a view model without sections. */
const DEFAULT_SECTION: Section = {
  id: "s0",
  page: { w: 11906, h: 16838, margins: { top: 1440, right: 1440, bottom: 1440, left: 1440 }, cols: 1 },
  headers: { default: null, first: null, even: null },
  footers: { default: null, first: null, even: null },
  titlePage: false,
};

const SAFE_ANCHOR = /^[a-z0-9._-]+$/;
const sel = (id: string) => `[data-qw-id="${id}"]`;

/** Keyed list of the top-level blocks of one story in one place on screen. */
class StoryView {
  nodes = new Map<string, HTMLElement>();
  constructor(
    readonly story: string,
    readonly target: (b: Block, i: number) => HTMLElement,
  ) {}

  /** Render `blocks`; nodes of ids not in `changed` are kept as they are. Returns new nodes. */
  sync(ctx: Ctx, blocks: Block[], changed: Set<string> | null): HTMLElement[] {
    const fresh: HTMLElement[] = [];
    const want = new Set(blocks.map((b) => b.id));
    for (const [id, n] of this.nodes)
      if (!want.has(id)) {
        n.remove();
        this.nodes.delete(id);
      }
    const groups = new Map<HTMLElement, HTMLElement[]>();
    for (const [i, b] of blocks.entries()) {
      let n = this.nodes.get(b.id);
      if (!n || !changed || changed.has(b.id)) {
        const old = n;
        n = renderTopBlock(ctx, b, this.story);
        if (old) old.replaceWith(n);
        this.nodes.set(b.id, n);
        fresh.push(n);
      }
      const box = this.target(b, i);
      let g = groups.get(box);
      if (!g) groups.set(box, (g = []));
      g.push(n);
    }
    // Put nodes in order with as few DOM moves as possible: untouched nodes stay where they are.
    for (const [box, list] of groups) {
      let cursor = box.firstChild;
      for (const n of list) {
        if (n === cursor) cursor = cursor.nextSibling;
        else box.insertBefore(n, cursor);
      }
      while (cursor) {
        const next = cursor.nextSibling;
        cursor.parentNode?.removeChild(cursor);
        cursor = next;
      }
    }
    return fresh;
  }
}

interface Frame {
  section: Section;
  el: HTMLElement;
  body: HTMLElement;
}

/** A rendered document. Created by `render()`. */
export class QuillworkView {
  readonly root: HTMLElement;
  private ctx: Ctx;
  private frames: Frame[] = [];
  private body!: StoryView;
  private storyViews = new Map<string, StoryView[]>();
  private docEl!: HTMLElement;
  private rail!: HTMLElement;
  private bar!: HTMLElement;
  private notes!: HTMLElement;
  private a11y!: HTMLElement;
  private pop!: HTMLElement;
  private popFor: HTMLElement | null = null;
  private popTimer = 0;
  private imageSel: ViewerSelection | null = null;
  private lastSelKey = "null";
  private cleanup: Array<() => void> = [];
  private layoutQueued = false;
  private flashTimers = new Set<ReturnType<typeof setTimeout>>();

  constructor(
    readonly container: HTMLElement,
    viewModel: ViewModel,
    private opts: ViewerOptions = {},
  ) {
    const doc = container.ownerDocument;
    if (opts.injectStyles !== false) injectStyles(doc);
    this.ctx = { doc, vm: cloneVm(viewModel), opts, uid: `qw${++instances}` };
    this.root = h(doc, "div", "qw-root");
    container.replaceChildren(this.root);
    this.setupTheme();
    this.listen();
    this.build();
  }

  /** Version of the document on screen. */
  get version(): number {
    return this.ctx.vm.version;
  }

  /** The view model as currently shown (after patches). */
  get viewModel(): ViewModel {
    return this.ctx.vm;
  }

  /** Replace the whole document (a full reload). */
  setViewModel(vm: ViewModel) {
    this.ctx.vm = cloneVm(vm);
    this.imageSel = null;
    this.build();
  }

  /** Update viewer options (callbacks, theme, ...) without re-rendering the document. */
  setOptions(opts: Partial<ViewerOptions>) {
    const theme = opts.theme !== undefined && opts.theme !== this.opts.theme;
    const hidden = opts.showHidden !== undefined && opts.showHidden !== this.opts.showHidden;
    const rebuild = (opts.assetUrl && opts.assetUrl !== this.opts.assetUrl) || (opts.rail !== undefined && opts.rail !== this.opts.rail);
    Object.assign(this.opts, opts);
    if (theme) this.setupTheme();
    if (hidden) this.root.classList.toggle("qw-nohidden", this.opts.showHidden === false);
    if (rebuild) this.build();
    else this.renderRail();
  }

  destroy() {
    for (const f of this.cleanup) f();
    this.cleanup = [];
    for (const t of this.flashTimers) clearTimeout(t);
    this.clearHighlight();
    this.root.remove();
  }

  // ---------------------------------------------------------------- building

  private build() {
    const { doc, vm } = this.ctx;
    this.root.replaceChildren();
    this.root.lang = vm.meta.lang;
    this.root.classList.toggle("qw-nohidden", this.opts.showHidden === false);
    this.bar = h(doc, "div", "qw-bar");
    const main = h(doc, "div", "qw-main");
    this.docEl = h(doc, "div", "qw-doc");
    this.docEl.setAttribute("role", "document");
    if (vm.meta.title) this.docEl.setAttribute("aria-label", vm.meta.title);
    this.rail = h(doc, "aside", "qw-rail");
    this.rail.setAttribute("aria-label", "Comments and changes");
    if (this.opts.rail === false) this.rail.hidden = true;
    this.notes = h(doc, "div", "qw-notes-wrap");
    this.a11y = h(doc, "div", "qw-sr");
    this.pop = h(doc, "div", "qw-pop");
    this.pop.hidden = true;
    this.pop.setAttribute("role", "group");
    main.append(this.docEl, this.rail);
    this.root.append(this.bar, main, this.a11y, this.pop);
    this.storyViews.clear();
    this.frames = [];
    this.body = new StoryView("b", (b, i) => this.bodyBoxFor(b, i));
    this.renderBar();
    this.buildFrames();
    this.body.sync(this.ctx, vm.blocks, null);
    this.docEl.append(this.notes);
    this.renderNotes();
    this.renderRail();
    this.updateLists();
    this.queueLayout();
  }

  private renderBar() {
    const { doc, vm } = this.ctx;
    const chips: HTMLElement[] = [];
    for (const l of vm.meta.labels) chips.push(h(doc, "span", "qw-chip", l));
    if (vm.meta.readOnly) chips.push(h(doc, "span", "qw-chip", `Read-only (${vm.meta.readOnly})`));
    const FLAG: Record<string, string> = {
      macros: "Contains macros",
      external_links: "External links",
      active_content: "Active content",
      suspicious_vba: "Suspicious macros",
      embedded_fonts: "Embedded fonts",
      hidden_text: "Hidden text",
      tiny_text: "Tiny text",
      white_text: "White text",
    };
    for (const f of vm.meta.flags) chips.push(h(doc, "span", "qw-chip qw-chip-warn", FLAG[f] ?? f));
    this.bar.replaceChildren(...chips);
    this.bar.hidden = !chips.length;
  }

  /** One page-like frame per section, with its header and footer. Body nodes are moved, not re-rendered. */
  private buildFrames() {
    const { doc, vm } = this.ctx;
    const old = this.frames;
    const sections = vm.sections.length ? vm.sections : [DEFAULT_SECTION];
    this.frames = sections.map((s, i) => {
      const el = h(doc, "section", "qw-sec");
      el.dataset.qwSec = s.id;
      if (i > 0 && s.breakType === "continuous") el.classList.add("qw-cont");
      const m = s.page.margins;
      const st = el.style;
      st.width = sectionPt(s.page.w);
      st.minHeight = sectionPt(s.page.h);
      const hdr = m.header ?? m.top / 2;
      const ftr = m.footer ?? m.bottom / 2;
      st.padding = `${sectionPt(hdr)} min(${sectionPt(m.right)}, 8%) ${sectionPt(ftr)} min(${sectionPt(m.left + (m.gutter ?? 0))}, 8%)`;
      const pick = (r: Section["headers"]) => (i === 0 && s.titlePage && r.first) || r.default;
      const headerEl = h(doc, "header", "qw-hdr");
      headerEl.style.minHeight = sectionPt(Math.max(0, m.top - hdr));
      headerEl.setAttribute("aria-label", "Header");
      const body = h(doc, "div", "qw-body");
      const footerEl = h(doc, "footer", "qw-ftr");
      footerEl.style.minHeight = sectionPt(Math.max(0, m.bottom - ftr));
      footerEl.setAttribute("aria-label", "Footer");
      el.append(headerEl, body, footerEl);
      this.mountStory(pick(s.headers), headerEl);
      this.mountStory(pick(s.footers), footerEl);
      return { section: s, el, body };
    });
    const ref = old[0]?.el ?? (this.notes.parentNode === this.docEl ? this.notes : null);
    for (const f of this.frames) this.docEl.insertBefore(f.el, ref);
    if (old.length) {
      this.body.sync(this.ctx, this.ctx.vm.blocks, new Set());
      for (const f of old) f.el.remove();
    }
  }

  private mountStory(id: string | null, box: HTMLElement) {
    if (!id) return;
    const story = this.ctx.vm.stories[id];
    if (!story) return;
    const v = new StoryView(id, () => box);
    let list = this.storyViews.get(id);
    if (!list) this.storyViews.set(id, (list = []));
    list.push(v);
    v.sync(this.ctx, story.blocks, null);
  }

  /**
   * The section frame a body block goes in: its own `section`, else the running section, which
   * moves on after a paragraph with `sectionEnd`.
   */
  private bodyBoxFor(b: Block, i: number): HTMLElement {
    if (i === 0) this.frameAt = 0;
    const own = b.section ? this.frames.findIndex((x) => x.section.id === b.section) : -1;
    if (own >= 0) this.frameAt = own;
    const f = this.frames[Math.min(this.frameAt, this.frames.length - 1)];
    if (b.kind === "p" && b.sectionEnd) this.frameAt++;
    return f.body;
  }
  private frameAt = 0;

  private renderNotes() {
    const { doc, vm } = this.ctx;
    for (const id of [...this.storyViews.keys()]) if (/^(fn|en|tb)\d/.test(id)) this.storyViews.delete(id);
    this.notes.replaceChildren();
    const groups: Array<[string, string, Story["kind"]]> = [
      ["fn", "Footnotes", "footnote"],
      ["en", "Endnotes", "endnote"],
      ["tb", "Text boxes", "textbox"],
    ];
    const num = (k: string) => parseInt(k.replace(/^\D+/, ""), 10);
    for (const [prefix, title, kind] of groups) {
      const ids = Object.keys(vm.stories)
        .filter((k) => k.startsWith(prefix) && /^\d/.test(k.slice(prefix.length)) && vm.stories[k].kind === kind)
        .sort((a, b) => num(a) - num(b));
      if (!ids.length) continue;
      const sec = h(doc, "section", `qw-notes qw-notes-${prefix}`);
      sec.setAttribute("aria-label", title);
      sec.append(h(doc, "div", "qw-notes-title", title));
      for (const id of ids) {
        const note = h(doc, "div", "qw-note");
        note.id = `${this.ctx.uid}-${id}`;
        note.append(h(doc, "span", "qw-note-label", prefix === "tb" ? `${num(id)}.` : String(num(id))));
        const box = h(doc, "div", "qw-note-body");
        note.append(box);
        sec.append(note);
        this.mountStory(id, box);
      }
      this.notes.append(sec);
    }
  }

  // ---------------------------------------------------------------- rail: comments and change sets

  private renderRail() {
    const { vm } = this.ctx;
    const items: HTMLElement[] = [];
    const canReview = !!this.opts.onReview && !vm.meta.readOnly;
    for (const cs of vm.changeSets) {
      if (cs.status !== "open" && cs.status !== "partial") continue;
      items.push(this.changeCard(cs, canReview));
    }
    const replies = new Map<string, Comment[]>();
    for (const c of vm.comments)
      if (c.parent) {
        let l = replies.get(c.parent);
        if (!l) replies.set(c.parent, (l = []));
        l.push(c);
      }
    for (const c of vm.comments) {
      if (c.parent && vm.comments.some((p) => p.id === c.parent)) continue;
      items.push(this.bubble(c, replies.get(c.id) ?? []));
    }
    this.rail.replaceChildren(...items);
    // Comment ranges in the text follow the comments' done state.
    for (const span of this.docEl.querySelectorAll<HTMLElement>("[data-qw-c]")) {
      const ids = (span.dataset.qwC ?? "").split(" ");
      span.classList.toggle("qw-cm-done", ids.every((id) => vm.comments.find((c) => c.id === id)?.done ?? true));
    }
    this.queueLayout();
  }

  private changeCard(cs: ChangeSet, canReview: boolean): HTMLElement {
    const doc = this.ctx.doc;
    const card = h(doc, "section", "qw-card");
    card.dataset.qwCard = cs.id;
    card.tabIndex = 0;
    const n = cs.revisions.length + (cs.untracked?.length ?? 0);
    const label = cs.label || "Change";
    card.setAttribute("aria-label", `Change set: ${label}, by ${cs.author}, ${n} change${n === 1 ? "" : "s"}`);
    const head = h(doc, "div", "qw-card-head");
    head.append(h(doc, "strong", "", label));
    const meta = h(doc, "div", "qw-meta", `${cs.author} · ${n} change${n === 1 ? "" : "s"}${cs.status === "partial" ? " · partly reviewed" : ""}`);
    card.append(head, meta);
    if (canReview) card.append(this.reviewButtons(cs.id, label));
    const on = (v: boolean) => this.markActive(`[data-qw-cs="${cs.id}"]`, v);
    card.addEventListener("mouseenter", () => on(true));
    card.addEventListener("mouseleave", () => on(false));
    card.addEventListener("focusin", () => on(true));
    card.addEventListener("focusout", () => on(false));
    return card;
  }

  private reviewButtons(changeId: string, what: string): HTMLElement {
    const doc = this.ctx.doc;
    const row = h(doc, "div", "qw-actions");
    for (const action of ["accept", "reject"] as const) {
      const b = h(doc, "button", `qw-btn qw-btn-${action}`, action === "accept" ? "Accept" : "Reject");
      b.type = "button";
      b.setAttribute("aria-label", `${action === "accept" ? "Accept" : "Reject"} ${what}`);
      b.dataset.qwAction = action;
      b.dataset.qwChange = changeId;
      row.append(b);
    }
    return row;
  }

  private bubble(c: Comment, replies: Comment[]): HTMLElement {
    const doc = this.ctx.doc;
    const art = h(doc, "article", "qw-bubble");
    art.dataset.qwComment = c.id;
    art.tabIndex = 0;
    if (c.done) art.classList.add("qw-done");
    art.setAttribute("aria-label", `Comment by ${c.author}${c.done ? ", resolved" : ""}${c.anchor ? ` on "${c.anchor.text}"` : ""}`);
    const one = (x: Comment, cls: string) => {
      const wrap = h(doc, "div", cls);
      const head = h(doc, "div", "qw-bubble-head");
      head.append(h(doc, "span", "qw-avatar", x.initials || x.author.slice(0, 2).toUpperCase()));
      head.append(h(doc, "strong", "", x.author));
      if (x.agent) head.append(h(doc, "span", "qw-badge", "AI"));
      head.append(h(doc, "time", "qw-meta", formatDate(x.date)));
      wrap.append(head);
      const body = h(doc, "div", "qw-bubble-body");
      for (const b of x.blocks) {
        const n = renderBlock(this.ctx, b);
        n.dataset.qwStory = x.id;
        body.append(n);
      }
      wrap.append(body);
      return wrap;
    };
    art.append(one(c, "qw-bubble-main"));
    for (const r of replies) art.append(one(r, "qw-reply"));
    if (c.done) art.append(h(doc, "div", "qw-meta", "Resolved"));
    const on = (v: boolean) => {
      art.classList.toggle("qw-active", v);
      this.markActive(`[data-qw-c~="${c.id}"]`, v, "qw-cm-active");
    };
    art.addEventListener("mouseenter", () => on(true));
    art.addEventListener("mouseleave", () => on(false));
    art.addEventListener("focusin", () => on(true));
    art.addEventListener("focusout", () => on(false));
    return art;
  }

  private markActive(selector: string, on: boolean, cls = "qw-rev-active") {
    for (const e of this.docEl.querySelectorAll(selector)) e.classList.toggle(cls, on);
  }

  /** Place rail items next to what they point at, without overlapping. */
  layout() {
    this.layoutQueued = false;
    if (this.rail.hidden) return;
    const railTop = this.rail.getBoundingClientRect().top;
    const items = Array.from(this.rail.children) as HTMLElement[];
    const placed = items.map((item, order) => {
      let target: Element | null = null;
      const cid = item.dataset.qwComment;
      const csid = item.dataset.qwCard;
      if (cid) {
        target = this.docEl.querySelector(`[data-qw-c~="${cid}"]`);
        const c = this.ctx.vm.comments.find((x) => x.id === cid);
        if (!target && c?.anchor && SAFE_ANCHOR.test(c.anchor.start)) target = this.docEl.querySelector(sel(c.anchor.start));
      } else if (csid) target = this.docEl.querySelector(`[data-qw-cs="${csid}"]`);
      const want = target ? target.getBoundingClientRect().top - railTop : Number.MAX_SAFE_INTEGER / 2 + order;
      return { item, want };
    });
    placed.sort((a, b) => a.want - b.want);
    let y = 0;
    for (const p of placed) {
      const top = Math.max(y, p.want >= Number.MAX_SAFE_INTEGER / 4 ? y : p.want);
      p.item.style.top = `${Math.round(top)}px`;
      y = top + p.item.offsetHeight + 8;
    }
    this.rail.style.minHeight = `${Math.round(y)}px`;
  }

  private queueLayout() {
    if (this.layoutQueued) return;
    this.layoutQueued = true;
    const raf = this.ctx.doc.defaultView?.requestAnimationFrame;
    if (raf) raf(() => this.layout());
    else queueMicrotask(() => this.layout());
  }

  // ---------------------------------------------------------------- accessibility: list semantics

  /** Runs of list paragraphs become ARIA lists (aria-owns), without wrapping the keyed block nodes. */
  private updateLists() {
    const doc = this.ctx.doc;
    this.a11y.replaceChildren();
    const containers = [...this.frames.map((f) => f.body), ...this.notes.querySelectorAll<HTMLElement>(".qw-note-body")];
    let n = 0;
    for (const box of containers) {
      let run: HTMLElement[] = [];
      const flush = () => {
        if (run.length) {
          const list = h(doc, "div");
          list.setAttribute("role", "list");
          list.setAttribute("aria-owns", run.map((e) => e.id).join(" "));
          this.a11y.append(list);
        }
        run = [];
      };
      for (const c of Array.from(box.children) as HTMLElement[]) {
        if (c.classList.contains("qw-li")) {
          if (run.length && run[0].dataset.qwList !== c.dataset.qwList) flush();
          c.id = `${this.ctx.uid}-l${++n}`;
          c.setAttribute("role", "listitem");
          run.push(c);
        } else flush();
      }
      flush();
    }
  }

  // ---------------------------------------------------------------- patches

  /**
   * Apply a view-model patch. Only blocks the patch names are re-rendered; every other node keeps
   * its identity. A patch for another base version changes nothing and asks for a full reload.
   */
  applyPatch(patch: Patch): PatchResult {
    const vm = this.ctx.vm;
    const fail = (reason: "version_mismatch" | "unknown_anchor" | "invalid_patch", detail: string): PatchResult => ({
      ok: false,
      needsReload: true,
      reason,
      detail,
      version: vm.version,
    });
    if (!patch || !Array.isArray(patch.removes) || !Array.isArray(patch.moves) || !Array.isArray(patch.upserts))
      return fail("invalid_patch", "removes, moves and upserts are required");
    if (patch.base_version !== vm.version)
      return fail("version_mismatch", `patch is for version ${patch.base_version}, viewer is at ${vm.version}`);

    // Work on copies of the story lists, so a failing patch changes nothing.
    // Order (patch.schema.json): removes, storyRemoves, moves, upserts, stories, ...
    const lists = new Map<string, Block[]>();
    const dropped = new Set<string>();
    const source = (story: string): Block[] | undefined => {
      if (story === "b") return vm.blocks;
      if (/^c\d/.test(story)) return vm.comments.find((c) => c.id === story)?.blocks;
      if (patch.stories?.[story]) return patch.stories[story].blocks;
      return dropped.has(story) ? undefined : vm.stories[story]?.blocks;
    };
    const list = (story: string): Block[] | undefined => {
      let l = lists.get(story);
      if (!l) {
        const s = source(story);
        if (!s) return undefined;
        lists.set(story, (l = s.slice()));
      }
      return l;
    };
    const allStories = () =>
      ["b", ...Object.keys(vm.stories), ...Object.keys(patch.stories ?? {}), ...vm.comments.map((c) => c.id)].filter(
        (s) => !dropped.has(s) || !!patch.stories?.[s],
      );
    const locate = (id: string): [string, number] | null => {
      for (const s of allStories()) {
        const l = lists.get(s) ?? source(s);
        const i = l ? l.findIndex((b) => b.id === id) : -1;
        if (i >= 0) return [s, i];
      }
      return null;
    };
    const insertAfter = (story: string, after: string | null, b: Block): string | null => {
      const l = list(story);
      if (!l) return `unknown story ${story}`;
      if (after === null) l.unshift(b);
      else {
        const i = l.findIndex((x) => x.id === after);
        if (i < 0) return `unknown anchor ${after} in story ${story}`;
        l.splice(i + 1, 0, b);
      }
      return null;
    };

    const changed = new Set<string>();
    const touched = new Set<string>(Object.keys(patch.stories ?? {}));
    for (const id of patch.removes) {
      const at = locate(id);
      if (!at) return fail("unknown_anchor", `cannot remove ${id}: not found`);
      list(at[0])!.splice(at[1], 1);
      touched.add(at[0]);
    }
    if (patch.storyRemoves && !Array.isArray(patch.storyRemoves)) return fail("invalid_patch", "storyRemoves must be a list");
    for (const k of patch.storyRemoves ?? []) {
      if (k === "b" || /^c\d/.test(k)) return fail("invalid_patch", `cannot remove story ${k}`);
      dropped.add(k);
      if (!patch.stories?.[k]) lists.delete(k);
    }
    for (const m of patch.moves) {
      const at = locate(m.id);
      if (!at) return fail("unknown_anchor", `cannot move ${m.id}: not found`);
      const [b] = list(at[0])!.splice(at[1], 1);
      const err = insertAfter(m.story, m.after, b);
      if (err) return fail("unknown_anchor", err);
      touched.add(at[0]).add(m.story);
    }
    for (const u of patch.upserts) {
      if (!u.block || typeof u.block.id !== "string") return fail("invalid_patch", "upsert without block id");
      const at = locate(u.block.id);
      if (at) {
        list(at[0])![at[1]] = u.block;
        touched.add(at[0]);
      } else {
        const err = insertAfter(u.story, u.after, u.block);
        if (err) return fail("unknown_anchor", err);
        touched.add(u.story);
      }
      changed.add(u.block.id);
    }

    // Commit.
    for (const k of dropped) if (!patch.stories?.[k]) delete vm.stories[k];
    for (const [story, l] of lists) {
      if (story === "b") vm.blocks = l;
      else if (/^c\d/.test(story)) {
        const c = vm.comments.find((x) => x.id === story);
        if (c) c.blocks = l;
      } else {
        const base = vm.stories[story] ?? patch.stories?.[story];
        vm.stories[story] = { kind: base!.kind, blocks: l };
      }
    }
    for (const [k, s] of Object.entries(patch.stories ?? {})) if (!lists.has(k)) vm.stories[k] = { kind: s.kind, blocks: s.blocks.slice() };
    if (patch.comments || patch.commentRemoves) {
      const drop = new Set(patch.commentRemoves ?? []);
      let cs = vm.comments.filter((c) => !drop.has(c.id));
      for (const c of patch.comments ?? []) {
        const i = cs.findIndex((x) => x.id === c.id);
        if (i >= 0) cs[i] = c;
        else cs = [...cs, c];
      }
      vm.comments = cs;
    }
    if (patch.commentOrder) {
      const rank = new Map(patch.commentOrder.map((id, i) => [id, i]));
      vm.comments = [...vm.comments].sort((a, b) => (rank.get(a.id) ?? Infinity) - (rank.get(b.id) ?? Infinity));
    }
    for (const c of patch.changeSets ?? []) {
      const i = vm.changeSets.findIndex((x) => x.id === c.id);
      if (i >= 0) vm.changeSets[i] = c;
      else vm.changeSets.push(c);
    }
    const fontsChanged = !!patch.meta && JSON.stringify(patch.meta.fonts ?? {}) !== JSON.stringify(vm.meta.fonts ?? {});
    if (patch.meta) vm.meta = patch.meta;
    if (patch.assets) Object.assign(vm.assets, patch.assets);
    if (patch.sections) vm.sections = patch.sections;
    vm.version = patch.version;

    if (fontsChanged) {
      // Every resolved font stack changes: re-render everything.
      this.build();
      return { ok: true, version: vm.version, changed: [...changed] };
    }

    // Update the DOM.
    const fresh: HTMLElement[] = [];
    if (patch.meta) {
      this.renderBar();
      this.root.lang = vm.meta.lang;
    }
    if (patch.sections || [...dropped].some((k) => /^(h|f)\d/.test(k))) {
      this.storyViews.forEach((_, id) => /^(h|f)\d/.test(id) && this.storyViews.delete(id));
      this.buildFrames();
    }
    for (const k of dropped) if (!vm.stories[k]) this.storyViews.delete(k);
    fresh.push(...this.body.sync(this.ctx, vm.blocks, changed));
    const newStory = Object.keys(patch.stories ?? {}).some((k) => !this.storyViews.has(k));
    if (newStory || dropped.size || patch.removes.length) this.renderNotes();
    for (const story of touched) {
      if (story === "b" || /^c\d/.test(story)) continue;
      const s = vm.stories[story];
      for (const v of this.storyViews.get(story) ?? []) if (s) fresh.push(...v.sync(this.ctx, s.blocks, changed));
    }
    this.renderRail();
    this.updateLists();
    this.flash(fresh);
    if (this.popFor && !this.popFor.isConnected) this.hidePop();
    this.queueLayout();
    return { ok: true, version: vm.version, changed: [...changed] };
  }

  private flash(nodes: HTMLElement[]) {
    const ms = this.opts.flashMs ?? 1200;
    if (!ms || !nodes.length) return;
    for (const n of nodes) {
      n.style.setProperty("--qw-flash-ms", `${ms}ms`);
      n.classList.add("qw-flash");
    }
    const t = setTimeout(() => {
      this.flashTimers.delete(t);
      for (const n of nodes) n.classList.remove("qw-flash");
    }, ms);
    this.flashTimers.add(t);
  }

  // ---------------------------------------------------------------- selection and pointing

  /** Find the element for an anchor (block, table, row, cell, image) or a comment id. */
  find(anchor: string): HTMLElement | null {
    if (!SAFE_ANCHOR.test(anchor)) return null;
    if (/^c\d+$/.test(anchor)) return this.rail.querySelector(`[data-qw-comment="${anchor}"]`);
    return this.docEl.querySelector(sel(anchor)) ?? this.rail.querySelector(sel(anchor));
  }

  /**
   * The current selection as anchors and accepted-mode offsets, or the clicked image.
   * Null when nothing in the document is selected.
   */
  getSelection(): ViewerSelection | null {
    const s = this.ctx.doc.getSelection();
    if (!s || s.rangeCount === 0) return this.imageSel;
    const r = s.getRangeAt(0);
    if (!this.docEl.contains(r.commonAncestorContainer)) return this.imageSel;
    const paraOf = (n: Node) => (n.nodeType === 1 ? (n as Element) : n.parentElement)?.closest<HTMLElement>('[data-qw-kind="p"]') ?? null;
    let a = paraOf(r.startContainer);
    let b = paraOf(r.endContainer);
    if (!a || !b) {
      const hits = Array.from(this.docEl.querySelectorAll<HTMLElement>('[data-qw-kind="p"]')).filter((p) => r.intersectsNode(p));
      a = a ?? hits[0] ?? null;
      b = b ?? hits[hits.length - 1] ?? null;
    }
    if (!a || !b || !this.docEl.contains(a)) return this.imageSel;
    const storyOf = (e: HTMLElement) => e.closest<HTMLElement>("[data-qw-story]")?.dataset.qwStory;
    const topOf = (e: HTMLElement) => e.closest<HTMLElement>("[data-qw-top]")?.dataset.qwId ?? e.dataset.qwId!;
    if (storyOf(a) !== storyOf(b)) return null; // a range never spans stories
    const start = a.contains(r.startContainer) ? offsetOf(a, r.startContainer, r.startOffset) : 0;
    const end = b.contains(r.endContainer) ? offsetOf(b, r.endContainer, r.endOffset) : blockText(b).length;
    const ida = a.dataset.qwId!;
    const idb = b.dataset.qwId!;
    let text: string;
    let anchor: string;
    if (a === b) {
      anchor = ida;
      text = blockText(a).slice(start, end);
    } else {
      const ta = topOf(a);
      const tb = topOf(b);
      anchor = ta === tb ? ta : `${ta}..${tb}`;
      const all = Array.from(this.docEl.querySelectorAll<HTMLElement>('[data-qw-kind="p"]'));
      const parts = all.slice(all.indexOf(a), all.indexOf(b) + 1).map((p) => blockText(p));
      parts[0] = parts[0].slice(start);
      parts[parts.length - 1] = parts[parts.length - 1].slice(0, end);
      text = parts.join("\n");
    }
    return { anchor, start, end, text, startBlock: ida, endBlock: idb };
  }

  private emitSelection() {
    const cur = this.getSelection();
    const key = JSON.stringify(cur);
    if (key === this.lastSelKey) return;
    this.lastSelKey = key;
    this.opts.onSelect?.(cur);
  }

  private selectImage(el: HTMLElement) {
    for (const e of this.root.querySelectorAll(".qw-selected")) e.classList.remove("qw-selected");
    el.classList.add("qw-selected");
    this.imageSel = { anchor: el.dataset.qwImg!, start: 0, end: 0, text: "" };
    this.ctx.doc.getSelection()?.removeAllRanges();
    this.emitSelection();
  }

  /** Mark an anchor, a range "a..b", or characters in one paragraph. Returns false if not found. */
  highlight(target: HighlightTarget): boolean {
    this.clearHighlight();
    if (typeof target === "string") {
      const [a, b] = target.split("..");
      const ea = this.find(a);
      if (!ea) return false;
      if (b === undefined) {
        ea.classList.add("qw-hl");
        return true;
      }
      const eb = this.find(b);
      if (!eb || ea.parentElement?.closest("[data-qw-top]")) {
        ea.classList.add("qw-hl");
        return !!eb;
      }
      // Every top-level block of the story from a to b.
      const story = ea.dataset.qwStory;
      const all = Array.from(this.docEl.querySelectorAll<HTMLElement>(`[data-qw-top][data-qw-story="${story}"]`));
      const i = all.indexOf(ea);
      const j = all.indexOf(eb);
      if (i < 0 || j < i) return false;
      for (const e of all.slice(i, j + 1)) e.classList.add("qw-hl");
      return true;
    }
    const el = this.find(target.anchor);
    if (!el) return false;
    const win = this.ctx.doc.defaultView as (Window & { CSS?: { highlights?: Map<string, unknown> }; Highlight?: new (r: Range) => unknown }) | null;
    if (el.dataset.qwKind === "p" && target.start != null && win?.CSS?.highlights && win.Highlight) {
      const r = this.ctx.doc.createRange();
      const s = pointAt(el, target.start);
      const e = pointAt(el, target.end ?? blockText(el).length);
      r.setStart(s.node, s.offset);
      r.setEnd(e.node, e.offset);
      win.CSS.highlights.set("qw-hl", new win.Highlight(r));
      el.classList.add("qw-hl-soft");
      return true;
    }
    el.classList.add("qw-hl");
    return true;
  }

  clearHighlight() {
    for (const e of this.root.querySelectorAll(".qw-hl,.qw-hl-soft")) e.classList.remove("qw-hl", "qw-hl-soft");
    const win = this.ctx.doc.defaultView as (Window & { CSS?: { highlights?: Map<string, unknown> } }) | null;
    win?.CSS?.highlights?.delete("qw-hl");
  }

  /** Scroll an anchor (or the start of a range) into view. Returns false if not found. */
  scrollTo(anchor: string): boolean {
    const el = this.find(anchor.split("..")[0]);
    if (!el) return false;
    const reduce = this.ctx.doc.defaultView?.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    el.scrollIntoView?.({ block: "center", behavior: reduce ? "auto" : "smooth" });
    return true;
  }

  // ---------------------------------------------------------------- events

  private setupTheme() {
    const win = this.ctx.doc.defaultView;
    const t = this.opts.theme ?? "auto";
    const mq = t === "auto" ? win?.matchMedia?.("(prefers-color-scheme: dark)") : undefined;
    const apply = () => this.root.classList.toggle("qw-dark", t === "dark" || (t === "auto" && !!mq?.matches));
    apply();
    this.root.dataset.qwTheme = t;
    if (mq) {
      mq.addEventListener?.("change", apply);
      this.cleanup.push(() => mq.removeEventListener?.("change", apply));
    }
  }

  private on<K extends keyof HTMLElementEventMap>(el: EventTarget, type: K | string, fn: (e: Event) => void) {
    el.addEventListener(type, fn);
    this.cleanup.push(() => el.removeEventListener(type, fn));
  }

  private listen() {
    const doc = this.ctx.doc;
    const win = doc.defaultView;
    this.on(doc, "selectionchange", () => {
      const s = doc.getSelection();
      if (s && s.rangeCount && !s.getRangeAt(0).collapsed && this.docEl?.contains(s.anchorNode)) {
        this.imageSel = null;
        for (const e of this.root.querySelectorAll(".qw-selected")) e.classList.remove("qw-selected");
      }
      if (s?.anchorNode && !this.root.contains(s.anchorNode) && this.lastSelKey === "null") return;
      this.emitSelection();
    });
    this.on(this.root, "click", (ev) => {
      const t = ev.target as HTMLElement;
      const btn = t.closest<HTMLElement>("[data-qw-action]");
      if (btn) {
        this.opts.onReview?.(btn.dataset.qwChange!, btn.dataset.qwAction as "accept" | "reject");
        this.hidePop();
        return;
      }
      const img = t.closest<HTMLElement>("[data-qw-img]");
      if (img && this.docEl.contains(img)) return this.selectImage(img);
      const cm = t.closest<HTMLElement>("[data-qw-c]");
      if (cm) {
        const id = (cm.dataset.qwC ?? "").split(" ")[0];
        const bubble = this.rail.querySelector<HTMLElement>(`[data-qw-comment="${id}"]`);
        for (const b of this.rail.querySelectorAll(".qw-bubble.qw-active")) b.classList.remove("qw-active");
        bubble?.classList.add("qw-active");
      }
    });
    this.on(this.root, "keydown", (ev) => {
      const e = ev as KeyboardEvent;
      const t = e.target as HTMLElement;
      if (t.dataset?.qwImg && (e.key === "Enter" || e.key === " ")) {
        e.preventDefault();
        this.selectImage(t);
      } else if (t.dataset?.qwRev && (e.key === "Enter" || e.key === "ArrowDown")) {
        this.showPop(t);
        const first = this.pop.querySelector<HTMLElement>("button");
        if (first) {
          e.preventDefault();
          first.focus();
        }
      } else if (e.key === "Escape" && this.pop.contains(t)) {
        const back = this.popFor;
        this.hidePop();
        back?.focus();
      }
    });
    const enter = (ev: Event) => {
      const t = (ev.target as HTMLElement).closest?.<HTMLElement>("[data-qw-rev]");
      if (t && this.docEl.contains(t)) this.showPop(t);
      else if (this.pop.contains(ev.target as Node)) clearTimeout(this.popTimer);
    };
    const leave = (ev: Event) => {
      const t = ev.target as HTMLElement;
      const next = (ev as FocusEvent).relatedTarget as Node | null;
      if (next && (this.pop.contains(next) || next === this.popFor)) return;
      if (t.closest?.("[data-qw-rev]") || this.pop.contains(t)) this.schedulePopHide();
    };
    this.on(this.root, "mouseover", enter);
    this.on(this.root, "focusin", enter);
    this.on(this.root, "mouseout", leave);
    this.on(this.root, "focusout", leave);
    if (win) {
      this.on(win, "resize", () => this.queueLayout());
      const RO = (win as Window & { ResizeObserver?: typeof ResizeObserver }).ResizeObserver;
      if (RO) {
        const ro = new RO(() => this.queueLayout());
        ro.observe(this.root);
        this.cleanup.push(() => ro.disconnect());
      }
      doc.fonts?.ready?.then(() => this.queueLayout()).catch(() => {});
    }
  }

  private showPop(target: HTMLElement) {
    clearTimeout(this.popTimer);
    if (this.popFor === target && !this.pop.hidden) return;
    const doc = this.ctx.doc;
    const vm = this.ctx.vm;
    this.popFor = target;
    const id = target.dataset.qwRev!;
    const cs = target.dataset.qwCs ? vm.changeSets.find((c) => c.id === target.dataset.qwCs) : undefined;
    const info = h(doc, "div", "qw-pop-info");
    info.append(h(doc, "span", "", target.title));
    if (cs?.label) info.append(h(doc, "span", "qw-meta", cs.label));
    this.pop.replaceChildren(info);
    this.pop.setAttribute("aria-label", target.title);
    if (this.opts.onReview && !vm.meta.readOnly) this.pop.append(this.reviewButtons(`r${id}`, "this change"));
    this.pop.hidden = false;
    const rr = this.root.getBoundingClientRect();
    const tr = target.getBoundingClientRect();
    this.pop.style.left = `${Math.max(0, Math.round(tr.left - rr.left))}px`;
    this.pop.style.top = `${Math.round(tr.bottom - rr.top + 4)}px`;
  }

  private schedulePopHide() {
    clearTimeout(this.popTimer);
    this.popTimer = setTimeout(() => this.hidePop(), 250) as unknown as number;
  }

  private hidePop() {
    clearTimeout(this.popTimer);
    this.pop.hidden = true;
    this.popFor = null;
  }
}

function cloneVm(vm: ViewModel): ViewModel {
  const stories: ViewModel["stories"] = {};
  for (const [k, s] of Object.entries(vm.stories ?? {})) stories[k] = { kind: s.kind, blocks: s.blocks.slice() };
  return {
    ...vm,
    blocks: vm.blocks.slice(),
    stories,
    comments: (vm.comments ?? []).map((c) => ({ ...c })),
    changeSets: (vm.changeSets ?? []).slice(),
    assets: { ...(vm.assets ?? {}) },
  };
}

/** Add the default stylesheet once per document. */
export function injectStyles(doc: Document) {
  if (doc.getElementById("qw-styles")) return;
  const style = doc.createElement("style");
  style.id = "qw-styles";
  style.textContent = css;
  (doc.head ?? doc.documentElement).prepend(style);
}

/** Render a view model into `container` (its children are replaced). */
export function render(container: HTMLElement, viewModel: ViewModel, options: ViewerOptions = {}): QuillworkView {
  return new QuillworkView(container, viewModel, options);
}
