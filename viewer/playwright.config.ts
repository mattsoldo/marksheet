import { defineConfig, devices } from "@playwright/test";

/**
 * Real-browser checks against the built viewer (`npm run build` first). They
 * assert behavior and accessibility rather than pixels, because fonts and
 * anti-aliasing differ between machines; screenshots of every theme and view
 * are attached to the HTML report for human review.
 */
export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  reporter: [["list"], ["html", { open: "never" }]],
  use: {
    baseURL: "http://localhost:4173",
    ...devices["Desktop Chrome"],
    viewport: { width: 1440, height: 900 },
    // The stylesheet shortens transitions to one frame under reduced motion,
    // which keeps screenshots stable; audits also wait for animations to end.
    contextOptions: { reducedMotion: "reduce" },
  },
  webServer: {
    command: "npm run preview -- --port 4173 --strictPort",
    url: "http://localhost:4173",
    reuseExistingServer: !process.env.CI,
  },
});
