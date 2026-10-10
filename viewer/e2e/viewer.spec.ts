import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";

const require = createRequire(import.meta.url);
const axePath = require.resolve("axe-core/axe.min.js");
const budget = fileURLToPath(new URL("../../examples/budget.ms", import.meta.url));
const large = fileURLToPath(new URL("./fixtures/large.ms", import.meta.url));
/** The most cells one window may render; see viewer/src/viewport.ts. */
const MAX_WINDOW_CELLS = 240 * 56;

async function openWorkbook(page: Page, path: string): Promise<void> {
  await page.setInputFiles("#file-input", path);
  await expect(page.locator("#grid")).toBeVisible();
  await expect(page.locator("#status")).toHaveClass(/status-ok/);
}

/** True when the row's header is on screen inside the scroll container. */
async function rowIsVisible(page: Page, row: number): Promise<boolean> {
  return page.evaluate((target) => {
    const shell = document.querySelector<HTMLElement>("#grid-shell")!;
    const header = document.querySelector<HTMLElement>(`.row-header[data-row="${target}"]`);
    if (!header) return false;
    const shellBox = shell.getBoundingClientRect();
    const box = header.getBoundingClientRect();
    return box.top >= shellBox.top && box.bottom <= shellBox.bottom;
  }, row);
}

/**
 * Animations advance per frame, so a transition started in this frame still
 * reports its old color: wait two frames, then for every animation to end.
 */
async function settleTransitions(page: Page): Promise<void> {
  await page.evaluate(async () => {
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    await Promise.all(document.getAnimations().map((animation) => animation.finished));
  });
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
});

test("fits a small workbook in the reading view and shows the full window in details", async ({ page }) => {
  await openWorkbook(page, budget);
  // The inputs sheet's content ends at G4; the fit adds a column and two rows.
  await expect(page.locator("#viewport-status")).toContainText("A1:H6");
  await expect(page.locator(".grid-cell")).toHaveCount(8 * 6);

  await page.click("#toggle-details");
  await expect(page.locator("#viewport-status")).not.toContainText("A1:H6");
  expect(await page.locator(".grid-cell").count()).toBeGreaterThan(8 * 6);
});

test("scrolls a tall sheet one bounded window at a time", async ({ page }) => {
  await openWorkbook(page, large);
  await page.locator("#grid-shell").hover();
  // The last data row is 230; the reading view stops two rows after it.
  await expect.poll(async () => {
    await page.mouse.wheel(0, 800);
    return rowIsVisible(page, 230);
  }, { timeout: 20_000 }).toBe(true);
  expect(await page.locator(".grid-cell").count()).toBeLessThanOrEqual(MAX_WINDOW_CELLS);
  await expect(page.locator(".row-header[data-row='233']")).toHaveCount(0);

  await expect.poll(async () => {
    await page.mouse.wheel(0, -800);
    return rowIsVisible(page, 1);
  }, { timeout: 20_000 }).toBe(true);
});

test("scrolls a wide sheet horizontally to its last column", async ({ page }) => {
  await openWorkbook(page, large);
  await page.click(".sheet-tab >> text=Wide");
  await expect(page.locator("#viewport-status")).toContainText("Wide");
  await page.locator("#grid-shell").hover();
  // Column 60 (BH) is the last data column.
  await expect.poll(async () => {
    await page.mouse.wheel(800, 0);
    return page.locator(".column-header[data-column='60']").count();
  }, { timeout: 20_000 }).toBe(1);
  expect(await page.locator(".grid-cell").count()).toBeLessThanOrEqual(MAX_WINDOW_CELLS);
});

test("keeps keyboard focus while arrowing past the rendered window", async ({ page }) => {
  await openWorkbook(page, large);
  await page.click(".grid-cell[data-coordinate='1:1']");
  for (let step = 0; step < 120; step += 1) await page.keyboard.press("ArrowDown");
  await expect(page.locator(".grid-cell[data-coordinate='1:121']")).toBeFocused();
  await expect(page.locator(".cell-selected")).toHaveAttribute("data-coordinate", "1:121");
  expect(await rowIsVisible(page, 121)).toBe(true);
  expect(await page.locator(".grid-cell").count()).toBeLessThanOrEqual(MAX_WINDOW_CELLS);
});

test("keeps arrow keys working after scrolling away from the focused cell", async ({ page }) => {
  await openWorkbook(page, large);
  await page.click(".grid-cell[data-coordinate='1:1']");
  await page.locator("#grid-shell").hover();
  // Scroll far enough that the window no longer contains A1.
  await expect.poll(async () => {
    await page.mouse.wheel(0, 800);
    return page.locator(".grid-cell[data-coordinate='1:1']").count();
  }, { timeout: 20_000 }).toBe(0);
  await expect(page.locator("#grid")).toBeFocused();
  expect(await page.locator(".grid-cell[tabindex='0']").count()).toBe(1);

  // While A1 is off-window its source is unknown, so it must not be editable as a blank.
  await page.click("#toggle-details");
  await expect(page.locator("#name-box")).toHaveValue("A1");
  await expect(page.locator("#formula-input")).toBeDisabled();
  await expect(page.locator("#formula-input")).toHaveValue("");
  await page.click("#toggle-details");

  await page.locator("#grid").focus();
  await page.keyboard.press("ArrowDown");
  await expect(page.locator(".grid-cell[data-coordinate='1:2']")).toBeFocused();
  expect(await rowIsVisible(page, 2)).toBe(true);
});

test("returns to the same place when Details closes and reopens", async ({ page }) => {
  await openWorkbook(page, budget);
  await page.click("#toggle-details");
  await page.fill("#name-box", "A120");
  await page.press("#name-box", "Enter");
  await expect.poll(() => rowIsVisible(page, 120)).toBe(true);

  // The reading view fits the budget's data, so row 120 is not shown there.
  await page.click("#toggle-details");
  await expect(page.locator("#viewport-status")).toContainText("A1:H6");

  await page.click("#toggle-details");
  await expect.poll(() => rowIsVisible(page, 120)).toBe(true);
});

test("has no accessibility violations in any theme or view", async ({ page }, testInfo) => {
  await openWorkbook(page, budget);
  await page.addScriptTag({ path: axePath });
  for (const theme of ["paper", "ledger", "graphite"]) {
    await page.click(`[data-theme-option="${theme}"]`);
    for (const details of [false, true]) {
      if (details) await page.click("#toggle-details");
      await expect(page.locator("#details-bar")).toBeVisible({ visible: details });
      await settleTransitions(page);
      const violations = await page.evaluate(async () => {
        const axe = (window as unknown as { axe: { run(context: Document): Promise<{ violations: Array<{ id: string; nodes: unknown[] }> }> } }).axe;
        const result = await axe.run(document);
        return result.violations.map((violation) => `${violation.id}: ${violation.nodes
          .map((node) => (node as { target: string[]; failureSummary?: string }).target.join(" ")
            + ` (${(node as { failureSummary?: string }).failureSummary?.split("\n")[1]?.trim() ?? ""})`)
          .join("; ")}`);
      });
      const view = details ? "details" : "reading";
      await testInfo.attach(`${theme}-${view}`, { body: await page.screenshot(), contentType: "image/png" });
      expect(violations, `${theme} ${view}`).toEqual([]);
      if (details) await page.click("#toggle-details");
    }
  }
});

test("keeps the phone layout within the screen", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 390, height: 780 });
  await page.reload();
  await openWorkbook(page, budget);
  await expect(page.locator("#app-shell")).toHaveAttribute("data-sidebar", "closed");
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await testInfo.attach("phone-reading", { body: await page.screenshot(), contentType: "image/png" });

  await page.click("#toggle-sidebar");
  await expect(page.locator("#app-shell")).toHaveAttribute("data-sidebar", "open");
  await expect(page.locator(".recent-name")).toHaveText(["budget.ms"]);
  await testInfo.attach("phone-sidebar", { body: await page.screenshot(), contentType: "image/png" });
});

test("remembers recent workbooks and preferences across a reload", async ({ page }) => {
  await openWorkbook(page, budget);
  await page.click('[data-theme-option="ledger"]');
  await expect(page.locator(".recent-name")).toHaveText(["budget.ms"]);

  await page.reload();
  await expect(page.locator("#app-shell")).toHaveAttribute("data-theme", "ledger");
  await expect(page.locator(".recent-name")).toHaveText(["budget.ms"]);
  await page.click(".recent-open");
  await expect(page.locator("#file-name")).toHaveText("budget.ms");
  await expect(page.locator("#grid")).toBeVisible();
  await expect(page).toHaveTitle("budget.ms — Marksheet");
});
