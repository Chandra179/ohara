import { expect, test } from "@playwright/test";

test("reports the five-process health contract", async ({ page }) => {
  const response = await page.request.get("/api/health");
  expect(response.ok()).toBe(true);
  const health = await response.json();

  expect(health.processes).toEqual({
    cleaning: expect.any(String),
    graph: expect.any(String),
    indexer: expect.any(String),
    retrieval: "available",
    scraper: expect.any(String),
  });
  expect(health.providers).toEqual({
    artifactStore: expect.any(String),
    embeddingModel: expect.any(String),
    falkordb: expect.any(String),
    ollama: expect.any(String),
    qdrant: expect.any(String),
  });
});

test("renders live Rust metrics without crashing Operations", async ({ page }) => {
  await page.goto("/operations");

  await expect(page.getByRole("heading", { exact: true, name: "Pipeline activity" })).toBeVisible();
  await expect(page.getByRole("heading", { exact: true, name: "LLM usage" })).toBeVisible();
});

test("renders a live query response", async ({ page }) => {
  await page.goto("/query");
  await page.getByRole("textbox", { name: "Ask your knowledge base" }).fill("What is Ohara?");
  await page.getByRole("button", { exact: true, name: "Ask" }).click();

  await expect(page.getByRole("button", { name: "Asking…" })).toBeHidden({ timeout: 30_000 });
  const result = page
    .getByRole("heading", { name: "Answer" })
    .or(page.getByText("No grounded answer"))
    .or(page.getByRole("heading", { name: "Local model unavailable" }));
  await expect(result).toBeVisible();
});

test("renders the live documents read model", async ({ page }) => {
  await page.goto("/documents");

  await expect(page.getByRole("heading", { name: "Documents" })).toBeVisible();
  await expect(page.getByRole("textbox", { name: "Search documents" })).toBeVisible();
});

test("renders the live entity review read model empty state", async ({ page }) => {
  await page.goto("/entities");

  await expect(page.getByRole("heading", { name: "Entities" })).toBeVisible();
  await expect(page.getByText("No entities need review")).toBeVisible();
});

test("searches and queues a topic from the live Overview", async ({ page }) => {
  await page.goto("/");

  await page.getByRole("textbox", { name: "Topic" }).fill("september 2026 news");
  await page.getByRole("spinbutton", { name: "Max articles" }).fill("2");
  const topicRequestPromise = page.waitForRequest(
    (request) => request.url().endsWith("/api/topics/scrape") && request.method() === "POST",
  );
  await page.getByRole("button", { exact: true, name: "Find & queue" }).click();
  const topicRequest = await topicRequestPromise;

  expect(topicRequest.postDataJSON()).toEqual({
    limit: 2,
    topic: "september 2026 news",
  });

  await expect(page.getByText(/results found for.*september 2026 news/)).toBeVisible();
  await expect(page.getByText(/queued.*already in your library/)).toBeVisible();
});
