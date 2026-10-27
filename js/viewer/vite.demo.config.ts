import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

// Serves demo/index.html; the example JSON is read from the repo's spec/ folder.
export default defineConfig({
  root: fileURLToPath(new URL("./demo", import.meta.url)),
  server: { fs: { allow: [fileURLToPath(new URL("../..", import.meta.url))] } },
});
