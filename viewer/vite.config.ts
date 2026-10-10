import { defineConfig } from "vitest/config";

export default defineConfig({
  // The website's Pages build serves the viewer beneath its own path with
  // relative asset URLs (`MARKSHEET_VIEWER_BASE=./`).
  base: process.env.MARKSHEET_VIEWER_BASE ?? "/",
  server: {
    fs: {
      // The production adapter imports the versioned client implementation
      // from bindings/wasm, one directory above this package.
      allow: [".."],
    },
  },
  test: {
    environment: "happy-dom",
    // Real-browser specs in e2e/ run under Playwright, not Vitest.
    include: ["tests/**/*.test.ts"],
    coverage: { reporter: ["text", "json-summary"] },
  },
});
