import { afterEach, describe, expect, it, vi } from "vitest";
import { act, createRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QuillworkViewer } from "../src/react";
import type { QuillworkView } from "../src/viewer";
import type { Patch } from "../src/types";
import { byId, examplePatch, mount, vm41, acceptPatch } from "./helpers";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | undefined;
afterEach(() => {
  act(() => root?.unmount());
  root = undefined;
});

describe("<QuillworkViewer>", () => {
  it("renders, applies appended patches once, and exposes the view", () => {
    const host = mount();
    root = createRoot(host);
    const ref = createRef<QuillworkView | null>();
    const vm = vm41();
    const onPatched = vi.fn();
    let patches: Patch[] = [];
    const ui = () => <QuillworkViewer viewModel={vm} patches={patches} ref={ref} onPatched={onPatched} flashMs={0} />;
    act(() => root!.render(ui()));
    expect(ref.current!.version).toBe(41);
    const b100 = byId(host, "b100");
    patches = [examplePatch()];
    act(() => root!.render(ui()));
    expect(ref.current!.version).toBe(42);
    expect(byId(host, "b100")).toBe(b100);
    patches = [...patches, acceptPatch()];
    act(() => root!.render(ui()));
    expect(ref.current!.version).toBe(43);
    expect(onPatched).toHaveBeenCalledTimes(2);
  });

  it("calls onNeedsReload for a patch that does not fit", () => {
    root = createRoot(mount());
    const onNeedsReload = vi.fn();
    act(() => root!.render(<QuillworkViewer viewModel={vm41()} patches={[acceptPatch()]} onNeedsReload={onNeedsReload} />));
    expect(onNeedsReload).toHaveBeenCalledWith(expect.objectContaining({ reason: "version_mismatch" }), expect.anything());
  });

  it("routes review clicks to the latest onReview", () => {
    const host = mount();
    root = createRoot(host);
    const first = vi.fn();
    const second = vi.fn();
    const vm = vm41();
    act(() => root!.render(<QuillworkViewer viewModel={vm} patches={[examplePatch()]} onReview={first} />));
    act(() => root!.render(<QuillworkViewer viewModel={vm} patches={[examplePatch()]} onReview={second} />));
    host.querySelector<HTMLButtonElement>('[data-qw-card="cs_7"] .qw-btn-accept')!.click();
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledWith("cs_7", "accept");
  });
});
