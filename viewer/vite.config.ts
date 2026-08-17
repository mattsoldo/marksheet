import { defineConfig } from "vite";

export default defineConfig({
  server: {
    fs: {
      // The production adapter imports the versioned client implementation
      // from bindings/wasm, one directory above this package.
      allow: [".."],
    },
  },
  test: {
    environment: "happy-dom",
    coverage: { reporter: ["text", "json-summary"] },
  },
});
