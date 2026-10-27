// Gzipped size of the built entry points. Fails when the core is over budget.
import { readFileSync } from "node:fs";
import { gzipSync } from "node:zlib";

const BUDGET = 30 * 1024;
let fail = false;
for (const f of ["dist/index.js", "dist/react.js"]) {
  const raw = readFileSync(new URL(`../${f}`, import.meta.url));
  const gz = gzipSync(raw, { level: 9 }).length;
  const line = `${f}: ${(raw.length / 1024).toFixed(1)} KB raw, ${(gz / 1024).toFixed(1)} KB gzip`;
  if (f === "dist/index.js" && gz > BUDGET) {
    fail = true;
    console.error(`${line}  OVER the ${BUDGET / 1024} KB budget`);
  } else console.log(line);
}
process.exit(fail ? 1 : 0);
