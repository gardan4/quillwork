import { afterEach, describe, expect, it } from "vitest";
import { render, type QuillworkView } from "../src/index";
import type { Patch } from "../src/types";
import { acceptPatch, byId, examplePatch, exampleVm, mount, vm41 } from "./helpers";

let view: QuillworkView | undefined;
afterEach(() => view?.destroy());

const topNodes = (v: QuillworkView) =>
  new Map(Array.from(v.root.querySelectorAll<HTMLElement>("[data-qw-top]")).map((e) => [e.dataset.qwId!, e]));
const bodyOrder = (v: QuillworkView) =>
  Array.from(v.root.querySelectorAll<HTMLElement>('.qw-body > [data-qw-top]')).map((e) => e.dataset.qwId);

describe("applyPatch", () => {
  it("replaces only the patched block; every other node keeps its identity", () => {
    view = render(mount(), vm41(), { flashMs: 50 });
    const before = topNodes(view);
    const order = bodyOrder(view);
    const r = view.applyPatch(examplePatch());
    expect(r).toEqual({ ok: true, version: 42, changed: ["b106"] });
    expect(view.version).toBe(42);
    const after = topNodes(view);
    expect([...after.keys()].sort()).toEqual([...before.keys()].sort());
    for (const [id, node] of after) {
      if (id === "b106") expect(node).not.toBe(before.get(id));
      else expect(node, id).toBe(before.get(id));
    }
    expect(bodyOrder(view)).toEqual(order);
    // The new node has the patched content and flashes.
    const b106 = byId(view.root, "b106")!;
    expect(b106.querySelector("ins")!.textContent).toBe("38");
    expect(b106.classList).toContain("qw-flash");
    expect(before.get("b105")!.classList).not.toContain("qw-flash");
    // The new change set shows up in the rail.
    expect(view.root.querySelector('[data-qw-card="cs_7"]')).not.toBeNull();
  });

  it("matches a fresh render after the patch", () => {
    view = render(mount(), vm41(), { flashMs: 0 });
    view.applyPatch(examplePatch());
    const patched = view.root.querySelector(".qw-doc")!.innerHTML.replace(/qw\d+-/g, "");
    view.destroy();
    view = render(mount(), exampleVm(), { flashMs: 0 });
    expect(view.root.querySelector(".qw-doc")!.innerHTML.replace(/qw\d+-/g, "")).toBe(patched);
  });

  it("applies a follow-up patch (accept result, 42 → 43) and an empty changeSets list", () => {
    view = render(mount(), vm41(), { flashMs: 0 });
    expect(view.applyPatch(examplePatch()).ok).toBe(true);
    const t1 = byId(view.root, "t1");
    const r = view.applyPatch(acceptPatch());
    expect(r).toEqual({ ok: true, version: 43, changed: ["b10f"] });
    expect(byId(view.root, "b10f")!.querySelector("ins")).toBeNull();
    expect(byId(view.root, "t1")).toBe(t1);
  });

  it("returns needs-reload on a version mismatch and changes nothing", () => {
    view = render(mount(), exampleVm());
    const html = view.root.innerHTML;
    const r = view.applyPatch(examplePatch()); // base 41, viewer at 42
    expect(r).toMatchObject({ ok: false, needsReload: true, reason: "version_mismatch", version: 42 });
    expect(view.version).toBe(42);
    expect(view.root.innerHTML).toBe(html);
  });

  it("returns needs-reload for an unknown anchor, without a partial update", () => {
    view = render(mount(), exampleVm());
    const html = view.root.innerHTML;
    const bad: Patch = {
      spec_version: "0.1.0",
      base_version: 42,
      version: 43,
      removes: ["b101"],
      moves: [],
      upserts: [{ story: "b", after: "b999", block: { id: "b200", kind: "p", style: "Normal", inlines: [{ k: "text", t: "x" }] } }],
    };
    expect(view.applyPatch(bad)).toMatchObject({ ok: false, reason: "unknown_anchor" });
    expect(view.version).toBe(42);
    expect(view.root.innerHTML).toBe(html);
    expect(view.viewModel.blocks.some((b) => b.id === "b101")).toBe(true);
  });

  it("inserts, removes and moves blocks", () => {
    view = render(mount(), exampleVm(), { flashMs: 0 });
    const before = topNodes(view);
    const r = view.applyPatch({
      spec_version: "0.1.0",
      base_version: 42,
      version: 43,
      removes: ["b104"],
      moves: [{ id: "b10h", story: "b", after: "b100" }],
      upserts: [
        { story: "b", after: null, block: { id: "b200", kind: "p", style: "Normal", inlines: [{ k: "text", t: "first" }] } },
        { story: "f1", after: "f1-00", block: { id: "f1-01", kind: "p", style: "Footer", inlines: [{ k: "text", t: "v2" }] } },
      ],
    });
    expect(r.ok).toBe(true);
    const order = bodyOrder(view);
    expect(order.slice(0, 3)).toEqual(["b200", "b100", "b10h"]);
    expect(order).not.toContain("b104");
    expect(byId(view.root, "b10h")).toBe(before.get("b10h")); // moved, not re-rendered
    expect(byId(view.root, "b100")).toBe(before.get("b100"));
    expect(view.root.querySelector(".qw-ftr")!.textContent).toBe("Pagina 1 van 2v2");
    expect(byId(view.root, "f1-00")).toBe(before.get("f1-00"));
  });

  it("adds a story whole and removes it with storyRemoves", () => {
    view = render(mount(), exampleVm(), { flashMs: 0 });
    const fn1 = { kind: "footnote" as const, blocks: [{ id: "fn1-00", kind: "p" as const, style: "FootnoteText", inlines: [{ k: "text" as const, t: "note" }] }] };
    const empty = { spec_version: "0.1.0", removes: [], moves: [], upserts: [] };
    expect(view.applyPatch({ ...empty, base_version: 42, version: 43, stories: { fn1 } }).ok).toBe(true);
    expect(view.root.querySelector(".qw-notes-fn")!.textContent).toContain("note");
    expect(view.applyPatch({ ...empty, base_version: 43, version: 44, storyRemoves: ["fn1"] }).ok).toBe(true);
    expect(view.root.querySelector(".qw-notes-fn")).toBeNull();
    expect(view.viewModel.stories.fn1).toBeUndefined();
    // A follow-up patch cannot reach blocks of the removed story.
    const r = view.applyPatch({ ...empty, base_version: 44, version: 45, removes: ["fn1-00"] });
    expect(r).toMatchObject({ ok: false, reason: "unknown_anchor" });
  });

  it("updates comments and change sets", () => {
    view = render(mount(), exampleVm(), { flashMs: 0 });
    const vm = exampleVm();
    const c1 = { ...vm.comments[0], done: true };
    const r = view.applyPatch({
      spec_version: "0.1.0",
      base_version: 42,
      version: 43,
      removes: [],
      moves: [],
      upserts: [],
      comments: [c1],
      changeSets: [{ ...vm.changeSets[0], status: "accepted" }],
    });
    expect(r.ok).toBe(true);
    expect(view.root.querySelector('[data-qw-card="cs_7"]')).toBeNull();
    expect(view.root.querySelector('[data-qw-comment="c1"]')!.classList).toContain("qw-done");
    expect(view.root.querySelector('[data-qw-c~="c1"]')!.classList).toContain("qw-cm-done");
  });

  it("puts a new comment where commentOrder says", () => {
    view = render(mount(), exampleVm(), { flashMs: 0 });
    const vm = exampleVm();
    const c0 = { ...vm.comments[0], id: "c0", done: false, blocks: vm.comments[0].blocks.map((b) => ({ ...b, id: "c0-0" })) };
    const r = view.applyPatch({
      spec_version: "0.1.0",
      base_version: 42,
      version: 43,
      removes: [],
      moves: [],
      upserts: [],
      comments: [c0],
      commentOrder: ["c0", ...vm.comments.map((c) => c.id)],
    });
    expect(r.ok).toBe(true);
    expect(view.viewModel.comments.map((c) => c.id)).toEqual(["c0", ...vm.comments.map((c) => c.id)]);
  });

  it("re-renders on a full reload", () => {
    view = render(mount(), exampleVm());
    const b100 = byId(view.root, "b100");
    view.setViewModel(vm41());
    expect(view.version).toBe(41);
    expect(byId(view.root, "b100")).not.toBe(b100);
  });
});
