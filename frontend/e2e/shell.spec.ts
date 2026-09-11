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

test("completes the entity merge journey", async ({ page }) => {
  await page.goto("/entities");

  await page.getByRole("button", { name: /Ohara Potential duplicate/ }).click();
  await expect(page.getByRole("heading", { exact: true, name: "Merge preview" })).toBeVisible();
  await page.getByRole("button", { exact: true, name: "Merge entities" }).click();
  await expect(page.getByRole("dialog", { name: "Confirm entity merge" })).toBeVisible();
  await page.getByRole("button", { exact: true, name: "Confirm merge" }).click();

  await expect(page.getByText("Merge completed", { exact: true })).toBeVisible();
  await expect(page.getByText("Entity merge completed")).toBeVisible();
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
