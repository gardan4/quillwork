import { afterEach, describe, expect, it, vi } from "vitest";
import { render, type QuillworkView } from "../src/index";
import type { Table, ViewModel } from "../src/types";
import { byId, exampleVm, mount } from "./helpers";

let view: QuillworkView | undefined;
afterEach(() => view?.destroy());

describe("render the example", () => {
  it("builds headings, paragraphs, list labels and resolved formatting", () => {
    view = render(mount(), exampleVm());
    const root = view.root;
    const b100 = byId(root, "b100")!;
    expect(b100.tagName).toBe("H1");
    expect(b100.textContent).toBe("Arbeidsovereenkomst voor bepaalde tijd");
    expect(byId(root, "b105")!.tagName).toBe("H2");
    expect(byId(root, "b101")!.tagName).toBe("P");
    // List paragraphs: visible label, list semantics through aria-owns.
    const b102 = byId(root, "b102")!;
    expect(b102.querySelector(".qw-label")!.textContent).toBe("1.");
    expect(b102.getAttribute("role")).toBe("listitem");
    const lists = root.querySelectorAll('.qw-sr [role="list"]');
    const owned = Array.from(lists).map((l) => l.getAttribute("aria-owns")!.split(" ").length);
    expect(owned).toContain(2); // b102 + b103
    // b10f: justified, Calibri mapped to Carlito, 11pt.
    const b10f = byId(root, "b10f")!;
    expect(b10f.style.textAlign).toBe("justify");
    expect(b10f.style.fontFamily).toContain("Carlito");
    expect(b10f.style.fontSize).toBe("11pt");
    expect(root.lang).toBe(view.viewModel.meta.lang);
  });

  it("renders the page frame from the section", () => {
    view = render(mount(), exampleVm());
    const sec = view.root.querySelector<HTMLElement>(".qw-sec")!;
    expect(sec.dataset.qwSec).toBe("s1");
    expect(sec.style.width).toBe("595.3pt"); // 11906 twips
  });

  it("renders the header image through assetUrl and the footer fields", () => {
    const assetUrl = vi.fn((id: string) => `https://cdn.test/${id}.png`);
    view = render(mount(), exampleVm(), { assetUrl });
    const img = view.root.querySelector<HTMLImageElement>(".qw-hdr img")!;
    expect(img.src).toBe("https://cdn.test/a_3c9e1f0a7b2d4e65.png");
    expect(img.alt).toBe("Logo Lindehof Kinderopvang");
    expect(assetUrl).toHaveBeenCalledWith("a_3c9e1f0a7b2d4e65", expect.objectContaining({ mime: "image/png" }));
    const fig = view.root.querySelector<HTMLElement>('[data-qw-id="img1"]')!;
    expect(fig.style.width).toBe("151px"); // 1440000 EMU
    expect(fig.classList).toContain("qw-align-right");
    const footer = view.root.querySelector(".qw-ftr")!;
    expect(footer.textContent).toBe("Pagina 1 van 2");
    expect(footer.querySelectorAll("[data-qw-field]").length).toBe(2);
  });

  it("renders the table with a header row, shading and cell anchors", () => {
    view = render(mount(), exampleVm());
    const t = byId(view.root, "t1")!;
    expect(t.tagName).toBe("TABLE");
    expect(t.querySelector("caption")!.textContent).toBe("Salaris");
    expect(t.querySelectorAll("thead th").length).toBe(3);
    expect(t.querySelectorAll("tbody td").length).toBe(3);
    expect(byId(t, "t1.r1.c1")!.style.getPropertyValue("--_bg")).toBe("#E8F0E0");
    expect(byId(t, "b10e")!.textContent).toBe("€ 2.912,00");
  });

  it("renders comments as margin bubbles and marks the commented text", () => {
    view = render(mount(), exampleVm());
    const bubble = view.root.querySelector<HTMLElement>('.qw-rail [data-qw-comment="c1"]')!;
    expect(bubble.tabIndex).toBe(0);
    expect(bubble.textContent).toContain("Klopt de functienaam met de cao?");
    expect(bubble.textContent).toContain("M. Jansen");
    const span = view.root.querySelector<HTMLElement>('[data-qw-c~="c1"]')!;
    expect(span.textContent).toBe("pedagogisch medewerker");
    bubble.dispatchEvent(new Event("mouseenter"));
    expect(span.classList).toContain("qw-cm-active");
  });

  it("shows document labels and flags, and hidden text dimmed", () => {
    view = render(mount(), exampleVm());
    const bar = view.root.querySelector(".qw-bar")!;
    expect(bar.textContent).toContain("Vertrouwelijk");
    expect(bar.textContent).toContain("Hidden text");
    expect(view.root.querySelector(".qw-f-hidden")!.textContent).toBe("concept v3");
    view.setOptions({ showHidden: false });
    expect(view.root.classList).toContain("qw-nohidden");
  });

  it("renders gridSpan and vMerge", () => {
    const vm: ViewModel = exampleVm();
    const p = (id: string, t: string) => ({ id, kind: "p" as const, style: "Normal", inlines: [{ k: "text" as const, t }] });
    const table: Table = {
      id: "t2",
      kind: "table",
      grid: [1000, 1000, 1000],
      rows: [
        { id: "t2.r1", cells: [{ id: "t2.r1.c1", span: 2, vmerge: null, blocks: [p("b200", "wide")] }, { id: "t2.r1.c2", span: 1, vmerge: "restart", blocks: [p("b201", "tall")] }] },
        { id: "t2.r2", cells: [{ id: "t2.r2.c1", span: 1, vmerge: null, blocks: [p("b202", "a")] }, { id: "t2.r2.c2", span: 1, vmerge: null, blocks: [p("b203", "b")] }, { id: "t2.r2.c3", span: 1, vmerge: "continue", blocks: [] }] },
        { id: "t2.r3", cells: [{ id: "t2.r3.c1", span: 1, vmerge: null, blocks: [p("b204", "c")] }, { id: "t2.r3.c2", span: 1, vmerge: null, blocks: [p("b205", "d")] }, { id: "t2.r3.c3", span: 1, vmerge: "continue", blocks: [] }] },
      ],
    };
    vm.blocks.push(table);
    view = render(mount(), vm);
    const t = byId(view.root, "t2")!;
    expect((byId(t, "t2.r1.c1") as HTMLTableCellElement).colSpan).toBe(2);
    expect((byId(t, "t2.r1.c2") as HTMLTableCellElement).rowSpan).toBe(3);
    expect(byId(t, "t2.r2.c3")).toBeNull();
    expect(t.querySelectorAll("td").length).toBe(6);
  });

  it("does not mutate the caller's view model", () => {
    const vm = exampleVm();
    const before = JSON.stringify(vm);
    view = render(mount(), vm);
    view.applyPatch({ spec_version: "0.1.0", base_version: 42, version: 43, removes: ["b101"], moves: [], upserts: [] });
    expect(JSON.stringify(vm)).toBe(before);
  });
});

describe("tracked changes", () => {
  it("marks insertions and deletions with author, date and change set", () => {
    view = render(mount(), exampleVm());
    const b106 = byId(view.root, "b106")!;
    const del = b106.querySelector<HTMLElement>("del.qw-rev")!;
    const ins = b106.querySelector<HTMLElement>("ins.qw-rev")!;
    expect(del.textContent).toBe("40");
    expect(ins.textContent).toBe("38");
    for (const e of [del, ins]) {
      expect(e.dataset.qwCs).toBe("cs_7");
      expect(e.dataset.qwAuthor).toBe("AI-assistent");
      expect(e.tabIndex).toBe(0);
      expect(e.title).toMatch(/AI-assistent/);
      expect(e.style.getPropertyValue("--_rc")).toMatch(/^var\(--qw-author-\d\)$/);
    }
    expect(del.dataset.qwRev).toBe("12");
    expect(ins.dataset.qwRev).toBe("13");
    expect(del.title).toMatch(/^Deleted by AI-assistent/);
    expect(ins.title).toMatch(/^Inserted by AI-assistent/);
    expect(del.classList).toContain("qw-del");
    expect(ins.classList).toContain("qw-ins");
    // A revision without a change set.
    const b10f = byId(view.root, "b10f")!.querySelector<HTMLElement>("ins")!;
    expect(b10f.textContent).toBe(" digitale");
    expect(b10f.dataset.qwCs).toBeUndefined();
    expect(b10f.dataset.qwAuthor).toBe("M. Jansen");
  });

  it("accept and reject a change set from its card", () => {
    const onReview = vi.fn();
    view = render(mount(), exampleVm(), { onReview });
    const card = view.root.querySelector<HTMLElement>('[data-qw-card="cs_7"]')!;
    expect(card.textContent).toContain("Arbeidsduur naar 38 uur");
    card.querySelector<HTMLButtonElement>(".qw-btn-accept")!.click();
    card.querySelector<HTMLButtonElement>(".qw-btn-reject")!.click();
    expect(onReview.mock.calls).toEqual([
      ["cs_7", "accept"],
      ["cs_7", "reject"],
    ]);
  });

  it("accept or reject one revision from the popover, by keyboard", () => {
    const onReview = vi.fn();
    view = render(mount(), exampleVm(), { onReview });
    const ins = byId(view.root, "b106")!.querySelector<HTMLElement>("ins")!;
    ins.focus();
    ins.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    const pop = view.root.querySelector<HTMLElement>(".qw-pop")!;
    expect(pop.hidden).toBe(false);
    expect(document.activeElement).toBe(pop.querySelector(".qw-btn-accept"));
    (document.activeElement as HTMLButtonElement).click();
    expect(onReview).toHaveBeenCalledWith("r13", "accept");
    expect(pop.hidden).toBe(true);
  });

  it("offers no review buttons without onReview or on a read-only document", () => {
    view = render(mount(), exampleVm());
    expect(view.root.querySelector(".qw-btn")).toBeNull();
    view.destroy();
    const vm = exampleVm();
    vm.meta.readOnly = "signed";
    view = render(mount(), vm, { onReview: () => {} });
    expect(view.root.querySelector(".qw-btn")).toBeNull();
  });
});
