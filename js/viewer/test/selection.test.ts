import { afterEach, describe, expect, it, vi } from "vitest";
import { render, blockText, pointAt, type QuillworkView } from "../src/index";
import { byId, exampleVm, mount } from "./helpers";

let view: QuillworkView | undefined;
afterEach(() => {
  view?.destroy();
  document.getSelection()?.removeAllRanges();
});

function select(a: HTMLElement, aOff: number, b: HTMLElement, bOff: number) {
  const s = pointAt(a, aOff);
  const e = pointAt(b, bOff);
  const r = document.createRange();
  r.setStart(s.node, s.offset);
  r.setEnd(e.node, e.offset);
  const sel = document.getSelection()!;
  sel.removeAllRanges();
  sel.addRange(r);
}

describe("text model", () => {
  it("counts accepted-mode text: deletions, list labels and markers count nothing", () => {
    view = render(mount(), exampleVm());
    expect(blockText(byId(view.root, "b106")!)).toBe(
      "Werknemer treedt op 1 maart 2027 in dienst als pedagogisch medewerker voor 38 uur per week.",
    );
    expect(blockText(byId(view.root, "b102")!)).toBe('Lindehof Kinderopvang B.V., gevestigd te Utrecht, hierna "werkgever";');
    expect(blockText(byId(view.root, "f1-00")!)).toBe("Pagina 1 van 2");
  });
});

describe("getSelection", () => {
  it("maps a selection inside one paragraph to anchor + offsets", () => {
    view = render(mount(), exampleVm());
    const b106 = byId(view.root, "b106")!;
    const text = blockText(b106);
    const start = text.indexOf("pedagogisch");
    select(b106, start, b106, start + "pedagogisch medewerker".length);
    expect(view.getSelection()).toEqual({
      anchor: "b106",
      start,
      end: start + 22,
      text: "pedagogisch medewerker",
      startBlock: "b106",
      endBlock: "b106",
    });
  });

  it("skips deleted text inside the selection", () => {
    view = render(mount(), exampleVm());
    const b106 = byId(view.root, "b106")!;
    const text = blockText(b106);
    const start = text.indexOf("voor");
    // A DOM range from "voor" to the end, which spans the <del>40</del>.
    const r = document.createRange();
    r.setStart(pointAt(b106, start).node, pointAt(b106, start).offset);
    r.setEnd(b106, b106.childNodes.length);
    document.getSelection()!.removeAllRanges();
    document.getSelection()!.addRange(r);
    const sel = view.getSelection()!;
    expect(sel.text).toBe("voor 38 uur per week.");
    expect(sel.start).toBe(start);
    expect(sel.end).toBe(text.length);
  });

  it("maps a selection across blocks to a range of top-level anchors", () => {
    view = render(mount(), exampleVm());
    const b101 = byId(view.root, "b101")!;
    const b103 = byId(view.root, "b103")!;
    select(b101, 3, b103, 2);
    const sel = view.getSelection()!;
    expect(sel.anchor).toBe("b101..b103");
    expect(sel.start).toBe(3);
    expect(sel.end).toBe(2);
    expect(sel.text.split("\n")).toEqual(["ondergetekenden:", blockText(byId(view.root, "b102")!), "P."]);
  });

  it("uses the table anchor for a selection inside one table, and table..block when it leaves it", () => {
    view = render(mount(), exampleVm());
    select(byId(view.root, "b109")!, 0, byId(view.root, "b10e")!, 4);
    expect(view.getSelection()).toMatchObject({ anchor: "t1", start: 0, end: 4, startBlock: "b109", endBlock: "b10e" });
    select(byId(view.root, "b10c")!, 0, byId(view.root, "b10f")!, 9);
    expect(view.getSelection()).toMatchObject({ anchor: "t1..b10f", end: 9 });
  });

  it("selects an image by click and by keyboard", () => {
    const onSelect = vi.fn();
    view = render(mount(), exampleVm(), { onSelect });
    const img = byId(view.root, "img1")!;
    img.click();
    expect(view.getSelection()).toEqual({ anchor: "img1", start: 0, end: 0, text: "" });
    expect(onSelect).toHaveBeenLastCalledWith({ anchor: "img1", start: 0, end: 0, text: "" });
    expect(img.classList).toContain("qw-selected");
    onSelect.mockClear();
    img.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(onSelect).not.toHaveBeenCalled(); // same selection, deduplicated
  });

  it("returns null when nothing in the document is selected", () => {
    view = render(mount(), exampleVm());
    expect(view.getSelection()).toBeNull();
  });
});

describe("highlight and scrollTo", () => {
  it("highlights an anchor, a range, and clears", () => {
    view = render(mount(), exampleVm());
    expect(view.highlight("b106")).toBe(true);
    expect(byId(view.root, "b106")!.classList).toContain("qw-hl");
    expect(view.highlight("b105..b107")).toBe(true);
    const hl = Array.from(view.root.querySelectorAll<HTMLElement>(".qw-hl")).map((e) => e.dataset.qwId);
    expect(hl).toEqual(["b105", "b106", "b107"]);
    expect(view.highlight("t1.r2.c3")).toBe(true);
    expect(view.root.querySelectorAll(".qw-hl").length).toBe(1);
    expect(view.highlight({ anchor: "b106", start: 0, end: 9 })).toBe(true);
    view.clearHighlight();
    expect(view.root.querySelector(".qw-hl")).toBeNull();
  });

  it("rejects unknown or unsafe anchors", () => {
    view = render(mount(), exampleVm());
    expect(view.highlight("b999")).toBe(false);
    expect(view.highlight('b"]')).toBe(false);
    expect(view.scrollTo("nope")).toBe(false);
  });

  it("scrolls to an anchor", () => {
    view = render(mount(), exampleVm());
    const el = byId(view.root, "b10f")!;
    const spy = vi.fn();
    el.scrollIntoView = spy;
    expect(view.scrollTo("b10f")).toBe(true);
    expect(spy).toHaveBeenCalled();
    expect(view.scrollTo("c1")).toBe(true);
  });
});
