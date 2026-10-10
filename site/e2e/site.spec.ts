import { createRequire } from "node:module";
import { expect, test, type Page } from "@playwright/test";

const require = createRequire(import.meta.url);
const axePath = require.resolve("axe-core/axe.min.js");

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

test("serves the full viewer beneath app/", async ({ page }) => {
  await page.goto("/app/");
  await expect(page.getByRole("heading", { name: "Open a Marksheet workbook" })).toBeVisible();
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
