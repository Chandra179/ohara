import { expect, test } from "@playwright/test";

test("loads the shared shell and navigates to Documents", async ({ page }) => {
  await page.goto("/");

  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
  await page.getByRole("link", { exact: true, name: "Documents" }).click();
  await expect(page.getByRole("heading", { exact: true, name: "Documents" })).toBeVisible();
});

test("completes the grounded query journey", async ({ page }) => {
  await page.goto("/query");

  await page.getByRole("textbox", { name: "Ask your knowledge base" }).fill("What is Ohara?");
  await page.getByRole("button", { exact: true, name: "Ask" }).click();

  await expect(page.getByRole("heading", { exact: true, name: "Answer" })).toBeVisible();
  await expect(page.getByRole("heading", { exact: true, name: "Citations" })).toBeVisible();
  await expect(page.getByText("ohara-product-guide.pdf")).toBeVisible();
  await expect(page.getByText("Query completed")).toBeVisible();
});

test("reviews an entity candidate without mutating the store", async ({ page }) => {
  await page.goto("/entities");

  await page.getByRole("button", { name: /Ohara.*Ohara/ }).click();
  await expect(page.getByRole("heading", { exact: true, name: "Candidate preview" })).toBeVisible();
  await expect(page.locator(".result-entity strong")).toHaveText("94.0% match");
  await expect(page.getByText("Review the candidates before taking an offline merge action.")).toBeVisible();
  await expect(page.getByRole("button", { exact: true, name: "Close preview" })).toBeVisible();
});

test("shows read-only Operations metrics", async ({ page }) => {
  await page.goto("/operations");

  await expect(page.getByRole("heading", { exact: true, name: "Operations" })).toBeVisible();
  await expect(page.getByRole("heading", { exact: true, name: "Pipeline activity" })).toBeVisible();
  await expect(page.getByRole("heading", { exact: true, name: "LLM usage" })).toBeVisible();
  await expect(page.getByText("Throughput and latency are not available yet")).toBeVisible();
});

test("supports keyboard entry and responsive document layouts", async ({ page }) => {
  await page.setViewportSize({ height: 800, width: 900 });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/documents");

  await page.keyboard.press("Tab");
  await expect(page.getByRole("link", { name: "Skip to content" })).toBeFocused();

  const layout = await page.evaluate(() => ({
    innerWidth: window.innerWidth,
    scrollWidth: document.documentElement.scrollWidth,
  }));
  expect(layout.scrollWidth).toBeLessThanOrEqual(layout.innerWidth);

  const transitionDuration = await page
    .getByRole("button", { name: "Refresh" })
    .evaluate((element) => Number.parseFloat(getComputedStyle(element).transitionDuration));
  expect(transitionDuration).toBeLessThanOrEqual(0.001);
});
