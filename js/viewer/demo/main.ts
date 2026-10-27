import { render } from "../src/index";
import type { Patch, ViewModel } from "../src/types";
import vmJson from "../../../spec/examples/view-model.json";
import patchJson from "../../../spec/examples/patch.json";
import acceptJson from "../../../spec/examples/results/op-result-accept.json";
// A data URL stands in for the asset server.
const LOGO = `data:image/svg+xml,${encodeURIComponent(
  '<svg xmlns="http://www.w3.org/2000/svg" width="472" height="177"><rect width="472" height="177" rx="16" fill="#E8F0E0"/><text x="236" y="105" font-family="sans-serif" font-size="44" text-anchor="middle" fill="#2E5E2A">Lindehof</text></svg>',
)}`;

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const log = (msg: string) => {
  const el = $("log");
  el.textContent = `${new Date().toLocaleTimeString()}  ${msg}\n${el.textContent}`;
};

/** The example is version 42, after the patch; rebuild version 41 so the patch has something to apply to. */
function version41(): ViewModel {
  const vm = structuredClone(vmJson) as unknown as ViewModel;
  vm.version = 41;
  const b106 = vm.blocks.find((b) => b.id === "b106");
  if (b106?.kind === "p") b106.inlines = [{ t: "Werknemer treedt op 1 maart 2027 in dienst voor 40 uur per week." }];
  vm.changeSets = [];
  return vm;
}

const view = render($("viewer"), version41(), {
  assetUrl: () => LOGO,
  onReview: (id, action) => log(`onReview(${JSON.stringify(id)}, ${JSON.stringify(action)})`),
  onSelect: (sel) => log(`onSelect ${JSON.stringify(sel)}`),
});
const status = () => ($("status").textContent = `version ${view.version}`);
status();

const apply = (p: Patch) => {
  const r = view.applyPatch(structuredClone(p));
  log(`applyPatch → ${JSON.stringify(r)}`);
  if (r.ok && r.changed[0]) view.scrollTo(r.changed[0]);
  status();
};
$("patch").onclick = () => apply(patchJson as unknown as Patch);
$("accept").onclick = () => apply((acceptJson as unknown as { patch: Patch }).patch);
$("stale").onclick = () => apply({ ...(patchJson as unknown as Patch), base_version: 7, version: 8 });
$("reset").onclick = () => {
  view.setViewModel(version41());
  status();
};
$<HTMLSelectElement>("theme").onchange = (e) => view.setOptions({ theme: (e.target as HTMLSelectElement).value as "auto" });
$<HTMLInputElement>("hl").onchange = (e) => {
  const v = (e.target as HTMLInputElement).value.trim();
  if (!v) return view.clearHighlight();
  log(`highlight(${v}) → ${view.highlight(v)}`);
  view.scrollTo(v);
};
Object.assign(window, { view });
