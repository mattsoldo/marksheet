import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";

const require = createRequire(import.meta.url);
const axePath = require.resolve("axe-core/axe.min.js");
const budget = fileURLToPath(new URL("../../examples/budget.ms", import.meta.url));

async function calculated(page: Page): Promise<void> {
  await expect(page.locator("[data-status]")).toHaveAttribute("data-state", "ok");
}

function cell(page: Page, coordinate: string) {
  return page.locator(`[data-grid] td[data-coordinate="${coordinate}"]`);
}

async function editSource(page: Page, edit: (source: string) => string): Promise<void> {
  const editor = page.getByLabel("Marksheet source");
  await editor.fill(edit(await editor.inputValue()));
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await calculated(page);
});

test("calculates the budget example with the real engine", async ({ page }) => {
  await expect(page.getByRole("tab", { name: "Inputs" })).toHaveAttribute("aria-selected", "true");
  // Subtotals come from a column fill, so they exist only once calculated.
  await expect(cell(page, "D2")).toHaveText("$1,500.00");
  await expect(cell(page, "G2")).toHaveText("20%");

  await page.getByRole("tab", { name: "Summary" }).click();
  await expect(cell(page, "B2")).toHaveText("$2,060.00");
  await expect(cell(page, "B4")).toHaveText("$1,648.00");
});

test("recalculates as the source is edited", async ({ page }) => {
  await editSource(page, (source) => source.replace("Rent|1500|1|", "Rent|1600|1|"));
  await expect(cell(page, "D2")).toHaveText("$1,600.00");
  await page.getByRole("tab", { name: "Summary" }).click();
  await expect(cell(page, "B2")).toHaveText("$2,160.00");
});

test("keeps the last good grid and explains a source error", async ({ page }) => {
  await editSource(page, (source) => source.replace("@sheet summary \"Summary\"", "@sheet summary"));
  await expect(page.locator("[data-status]")).toHaveAttribute("data-state", "error");
  await expect(page.locator("[data-diagnostics]")).toBeVisible();
  await expect(page.locator("[data-diagnostics]")).toContainText("MS1101");
  await expect(page.locator("[data-highlight] .has-error").first()).toHaveText("@sheet summary");
  await expect(cell(page, "D2")).toHaveText("$1,500.00");

  await editSource(page, (source) => source.replace("@sheet summary\n", "@sheet summary \"Summary\"\n"));
  await calculated(page);
  await expect(page.locator("[data-diagnostics]")).toBeHidden();
});

test("never understates diagnostics the engine truncated", async ({ page }) => {
  // 26 × 120 self-references: 3,120 cycles, past the worker's 1,000-diagnostic cap, all
  // inside the playground's reading range so the calculation reports them too.
  const columns = Array.from({ length: 26 }, (_, index) => String.fromCharCode(65 + index));
  const rows = Array.from({ length: 120 }, (_, row) => columns.map((column) => `=${column}${row + 1}`).join("|"));
  await page.getByLabel("Marksheet source").fill(`#!marksheet 0.1\n@sheet s "S"\n@block A1 pipe\n${rows.join("\n")}\n@end\n`);
  await expect(page.locator("[data-status]")).toContainText("Calculated with at least 3120 diagnostics");
  await expect(page.locator("[data-diagnostics] .diagnostic-more")).toHaveText("and at least 3116 more");
});

test("shows authored values when a workbook cannot be calculated", async ({ page }) => {
  // A required extension that isn't available leaves the workbook viewable but not calculable.
  await page.getByLabel("Marksheet source").fill(
    '#!marksheet 0.1\n@require actuarial_functions@1\n@sheet s "Premiums"\n@block A1 pipe\nItem|Cost\nBase|4.5\nTotal|=B2*2\n@end\n',
  );
  await expect(page.locator("[data-status]")).toContainText("Not calculated");
  await expect(page.getByRole("tab", { name: "Premiums" })).toBeVisible();
  await expect(cell(page, "B2")).toHaveText("4.5");
  await expect(cell(page, "B3")).toHaveText("=B2*2");
  await expect(page.locator("[data-diagnostics]")).toContainText("MS3101");
  // Screen readers must hear the same status as the page shows.
  await expect(page.locator("[data-grid] caption")).toHaveText("Premiums, authored values, not calculated");
});

test("highlights an @end inside a quoted field as data", async ({ page }) => {
  await page.getByLabel("Marksheet source").fill(
    '#!marksheet 0.1\n@sheet s "S"\n@block A1 csv\nNote,Value\n"first\n@end\nlast",1\n@end\n',
  );
  await calculated(page);
  const lines = page.locator("[data-highlight] .line");
  await expect(lines.nth(5)).toHaveText("@end");
  await expect(lines.nth(5).locator(".tok-directive")).toHaveCount(0);
  await expect(lines.nth(7).locator(".tok-directive")).toHaveText("@end");
});

test("links cells to the source lines that wrote them", async ({ page }) => {
  await cell(page, "G2").click();
  await expect(page.locator("[data-cell-ref]")).toHaveText("G2");
  await expect(page.locator("[data-highlight] .is-marked")).toHaveText("Tax rate|0.2");

  await cell(page, "D3").click();
  await expect(page.locator("[data-cell-source]")).toHaveAttribute("data-note", /filled/);
  await expect(page.locator("[data-highlight] .is-marked")).toContainText("@fill costs[Subtotal]");

  // The other direction: placing the caret in a row selects its cell.
  const editor = page.getByLabel("Marksheet source");
  const offset = (await editor.inputValue()).indexOf("Groceries|360") + 2;
  await editor.evaluate((element: HTMLTextAreaElement, at) => {
    element.focus();
    element.setSelectionRange(at, at);
  }, offset);
  await editor.press("ArrowRight");
  await expect(page.locator("[data-cell-ref]")).toHaveText("A4");
});

test("loads another example workbook", async ({ page }) => {
  await page.getByLabel("Example workbook").selectOption({ label: "Invoice" });
  await expect(page.locator("[data-file-name]")).toHaveText("invoice-basic.ms");
  await calculated(page);
  await expect(page.getByRole("tab", { name: "Invoice" })).toBeVisible();
  await expect(cell(page, "D8")).toHaveText("$375.00");
});

test("keeps keyboard focus on the sheet tabs", async ({ page }) => {
  await page.getByRole("tab", { name: "Inputs" }).focus();
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("tab", { name: "Summary" })).toHaveAttribute("aria-selected", "true");
  await expect(cell(page, "B2")).toHaveText("$2,060.00");
  // The tabs are rebuilt after the sheet loads; focus must follow the active one.
  await expect(page.getByRole("tab", { name: "Summary" })).toBeFocused();
  await page.keyboard.press("ArrowLeft");
  await expect(page.getByRole("tab", { name: "Inputs" })).toHaveAttribute("aria-selected", "true");
  await expect(page.getByRole("tab", { name: "Inputs" })).toBeFocused();
});

test("does not mark source lines from an older grid", async ({ page }) => {
  // A rejected edit that shifts every line keeps the previous grid on screen.
  await editSource(page, (source) => source.replace("#!marksheet 0.1\n", "#!marksheet 0.1\n@bogus\n"));
  await expect(page.locator("[data-status]")).toHaveAttribute("data-state", "error");
  await cell(page, "G2").click();
  await expect(page.locator("[data-cell-ref]")).toHaveText("G2");
  await expect(page.locator("[data-highlight] .is-marked")).toHaveCount(0);
});

test("starts a fresh worker after the engine crashes", async ({ page }) => {
  let crashes = 1;
  await page.route("**/marksheet-wasm/web/worker.js", (route) => (
    crashes-- > 0
      ? route.fulfill({ contentType: "text/javascript", body: "throw new Error('simulated crash');" })
      : route.continue()
  ));
  await page.goto("/");
  await expect(page.locator("[data-status]")).toHaveAttribute("data-state", "error");
  await editSource(page, (source) => source.replace("Rent|1500|1|", "Rent|1700|1|"));
  await calculated(page);
  await expect(cell(page, "D2")).toHaveText("$1,700.00");
});

test("starts a fresh worker after the engine fails to load", async ({ page }) => {
  // A failed module download makes the worker answer every request with a session error.
  let failures = 1;
  await page.route("**/marksheet_wasm_bg.wasm", (route) => (failures-- > 0 ? route.abort() : route.continue()));
  await page.goto("/");
  await expect(page.locator("[data-status]")).toHaveAttribute("data-state", "error");
  await editSource(page, (source) => source.replace("Rent|1500|1|", "Rent|1800|1|"));
  await calculated(page);
  await expect(cell(page, "D2")).toHaveText("$1,800.00");
});

test("starts a fresh worker after a revision mismatch", async ({ page }) => {
  // A worker out of step with the page rejects every request as stale; only a new one recovers.
  const stale = `self.onmessage = (event) => self.postMessage({
    protocol: "marksheet-worker@1", request_id: event.data.request_id, revision: 0,
    response: { kind: "error", error: { code: "stale_revision", message: "stale revision", diagnostics: [], diagnostics_omitted: 0 } },
  });`;
  let mismatches = 1;
  await page.route("**/marksheet-wasm/web/worker.js", (route) => (
    mismatches-- > 0 ? route.fulfill({ contentType: "text/javascript", body: stale }) : route.continue()
  ));
  await page.goto("/");
  await expect(page.locator("[data-status]")).toHaveAttribute("data-state", "error");
  await editSource(page, (source) => source.replace("Rent|1500|1|", "Rent|1900|1|"));
  await calculated(page);
  await expect(cell(page, "D2")).toHaveText("$1,900.00");
});

test("serves the full viewer beneath app/ with a working engine", async ({ page }) => {
  await page.goto("/app/");
  await expect(page.getByRole("heading", { name: "Open a Marksheet workbook" })).toBeVisible();
  // The viewer's worker starts only when a workbook opens, so open one to prove its asset URL.
  await page.setInputFiles("#file-input", budget);
  await expect(page.locator("#status")).toHaveClass(/status-ok/);
  await expect(page.locator('.grid-cell[data-coordinate="4:2"]')).toHaveText("$1,500.00");
});

for (const colorScheme of ["light", "dark"] as const) {
  test(`has no detectable accessibility violations in ${colorScheme} mode`, async ({ page }, testInfo) => {
    await page.emulateMedia({ colorScheme });
    await page.goto("/");
    await calculated(page);
    await page.addScriptTag({ path: axePath });
    const violations = await page.evaluate(async () => {
      const axe = (window as unknown as { axe: { run(): Promise<{ violations: Array<{ id: string; nodes: Array<{ target: string[] }> }> }> } }).axe;
      const result = await axe.run();
      return result.violations.map((violation) => `${violation.id}: ${violation.nodes.map((node) => node.target.join(" ")).join(", ")}`);
    });
    await testInfo.attach(`site-${colorScheme}.png`, { body: await page.screenshot({ fullPage: true }), contentType: "image/png" });
    expect(violations).toEqual([]);
  });
}

test("fits a phone screen without horizontal scrolling", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await calculated(page);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
  await testInfo.attach("site-phone.png", { body: await page.screenshot({ fullPage: true }), contentType: "image/png" });
});
