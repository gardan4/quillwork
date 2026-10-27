import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { Patch, ViewModel } from "../src/types";

const SPEC = resolve(dirname(fileURLToPath(import.meta.url)), "../../../spec");
export const specPath = (p: string) => resolve(SPEC, p);
export const readJson = <T>(p: string): T => JSON.parse(readFileSync(specPath(p), "utf8")) as T;

/** The example view model (version 42). A fresh copy per call. */
export const exampleVm = (): ViewModel => readJson<ViewModel>("examples/view-model.json");
/** The example patch, 41 → 42. */
export const examplePatch = (): Patch => readJson<Patch>("examples/patch.json");
/** The patch in the accept example result, 42 → 43. */
export const acceptPatch = (): Patch => readJson<{ patch: Patch }>("examples/results/op-result-accept.json").patch;

/** The example as it was at version 41: what the example patch applies to. */
export function vm41(): ViewModel {
  const vm = exampleVm();
  vm.version = 41;
  const b106 = vm.blocks.find((b) => b.id === "b106");
  if (b106 && b106.kind === "p") b106.inlines = [{ k: "text", t: "Werknemer treedt op 1 maart 2027 in dienst voor 40 uur per week." }];
  vm.changeSets = [];
  return vm;
}

export function mount(): HTMLElement {
  document.body.innerHTML = "";
  const el = document.createElement("div");
  document.body.append(el);
  return el;
}

export const byId = (root: ParentNode, id: string) => root.querySelector<HTMLElement>(`[data-qw-id="${id}"]`);
