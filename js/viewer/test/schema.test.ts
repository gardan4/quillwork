import { describe, expect, it } from "vitest";
import Ajv2020 from "ajv/dist/2020";
import addFormats from "ajv-formats";
import { readdirSync } from "node:fs";
import { readJson, specPath } from "./helpers";
import type { Patch } from "../src/types";

const ajv = new Ajv2020({ allErrors: true, strict: false });
addFormats(ajv);
for (const f of ["common", "view-model", "patch"]) ajv.addSchema(readJson(`schema/${f}.schema.json`));
const validator = (f: string) => {
  const id = readJson<{ $id: string }>(`schema/${f}.schema.json`).$id;
  return ajv.getSchema(id)!;
};

describe("spec examples validate", () => {
  it("view-model.json", () => {
    const v = validator("view-model");
    const ok = v(readJson("examples/view-model.json"));
    expect(v.errors ?? []).toEqual([]);
    expect(ok).toBe(true);
  });

  it("patch.json", () => {
    const v = validator("patch");
    const ok = v(readJson("examples/patch.json"));
    expect(v.errors ?? []).toEqual([]);
    expect(ok).toBe(true);
  });

  it("every patch inside the example results", () => {
    const v = validator("patch");
    const dir = specPath("examples/results");
    let n = 0;
    for (const f of readdirSync(dir)) {
      const r = readJson<{ patch?: Patch | null }>(`examples/results/${f}`);
      if (!r.patch) continue;
      n++;
      expect(v(r.patch), `${f}: ${ajv.errorsText(v.errors)}`).toBe(true);
    }
    expect(n).toBeGreaterThan(0);
  });
});
