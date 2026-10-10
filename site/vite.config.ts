import { defineConfig } from "vite";

export default defineConfig({
  // Relative asset URLs let the same build serve from a domain root or a
  // GitHub Pages project path such as /marksheet/.
  base: "./",
  server: {
    fs: {
      // The playground reuses the viewer's presentation code, the Wasm
      // client from bindings/wasm, and the example workbooks.
      allow: [".."],
    },
  },
});
