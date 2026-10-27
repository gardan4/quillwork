import { defineConfig } from "vitest/config";

export default defineConfig({
  build: {
    target: "es2022",
    minify: "esbuild",
    sourcemap: true,
    lib: {
      entry: { index: "src/index.ts", react: "src/react.tsx" },
      formats: ["es"],
    },
    rollupOptions: {
      external: ["react", "react/jsx-runtime", "react-dom"],
    },
  },
  test: {
    environment: "jsdom",
    include: ["test/**/*.test.ts", "test/**/*.test.tsx"],
  },
});
