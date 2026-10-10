import { defineConfig, devices } from "@playwright/test";

/**
 * Real-browser checks against the assembled Pages site (`npm run build:pages`
 * first): the playground runs the real Wasm engine, and the viewer is served
 * beneath app/.
 */
export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  reporter: [["list"], ["html", { open: "never" }]],
  use: {
    baseURL: "http://localhost:4174",
    ...devices["Desktop Chrome"],
    viewport: { width: 1440, height: 900 },
    contextOptions: { reducedMotion: "reduce" },
  },
  webServer: {
    command: "npm run preview -- --port 4174 --strictPort",
    url: "http://localhost:4174",
    reuseExistingServer: !process.env.CI,
  },
});
