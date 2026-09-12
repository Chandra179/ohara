import { expect, test } from "@playwright/test";

test("shows actionable readiness diagnostics for a missing embedding model", async ({ page }) => {
  const response = await page.request.get("/api/health");
  expect(response.ok()).toBe(true);
  const health = await response.json();

  expect(health.embedder).toBe("unavailable");
  expect(health.diagnostics).toEqual(
    expect.arrayContaining([
      expect.objectContaining({
        action: expect.stringContaining("download"),
        component: "embedder",
      }),
    ]),
  );

  await page.goto("/");
  await expect(page.getByText("Local readiness needs attention")).toBeVisible();
  await expect(page.getByText(/pinned embedding model is not downloaded/)).toBeVisible();
});

test("renders live Rust metrics without crashing Operations", async ({ page }) => {
  await page.goto("/operations");

  await expect(page.getByRole("heading", { exact: true, name: "Pipeline activity" })).toBeVisible();
  await expect(page.getByRole("heading", { exact: true, name: "LLM usage" })).toBeVisible();
  await expect(page.getByText("Local readiness needs attention")).toBeVisible();
});

test("renders a live query failure as an explicit error state", async ({ page }) => {
  await page.goto("/query");
  await page.getByRole("textbox", { name: "Ask your knowledge base" }).fill("What is Ohara?");
  await page.getByRole("button", { exact: true, name: "Ask" }).click();

  await expect(page.getByRole("heading", { name: "Something went wrong" })).toBeVisible();
  await expect(page.locator(".error-state").getByText(/pinned embedding model is not downloaded/)).toBeVisible();
});

test("renders the live documents read model empty state", async ({ page }) => {
  await page.goto("/documents");

  await expect(page.getByRole("heading", { name: "Documents" })).toBeVisible();
  await expect(page.getByText("No documents yet")).toBeVisible();
  await expect(page.getByRole("button", { name: "Next" })).toBeDisabled();
});

test("renders the live entity review read model empty state", async ({ page }) => {
  await page.goto("/entities");

  await expect(page.getByRole("heading", { name: "Entities" })).toBeVisible();
  await expect(page.getByText("No entities need review")).toBeVisible();
});

test("searches and queues a topic from the live Overview", async ({ page }) => {
  await page.goto("/");

  await page.getByRole("textbox", { name: "Topic" }).fill("september 2026 news");
  await page.getByRole("button", { exact: true, name: "Find & queue" }).click();

  await expect(page.getByText(/results found for.*september 2026 news/)).toBeVisible();
  await expect(page.getByText(/queued.*already in your library/)).toBeVisible();
});

test("reports an invalid knowledge artifact from the live Rust API", async ({ page }) => {
  const response = await page.request.get("http://127.0.0.1:4314/api/health");
  expect(response.ok()).toBe(true);
  const health = await response.json();

  expect(health.knowledgeStore).toBe("unavailable");
  expect(health.diagnostics).toEqual(
    expect.arrayContaining([expect.objectContaining({ component: "knowledgeStore" })]),
  );
});
