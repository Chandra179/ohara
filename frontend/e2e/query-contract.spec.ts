import { expect, test } from "@playwright/test";

const queryContractCases = [
  {
    expected: "No grounded answer",
    name: "renders an empty model result as available but ungrounded",
    payload: {
      answer: null,
      availability: "available",
      citations: [],
      chunks: [{ chunkId: "chunk-1", score: 0.9, text: "Evidence" }],
      grounding: "ungrounded",
    },
    role: "text",
  },
  {
    expected: "Local model unavailable",
    name: "renders an unavailable Ollama result without citations",
    payload: {
      answer: null,
      availability: "unavailable",
      citations: [],
      chunks: [{ chunkId: "chunk-1", score: 0.9, text: "Evidence" }],
      grounding: "ungrounded",
    },
    role: "heading",
  },
  {
    expected: "Something went wrong",
    name: "rejects a malformed model response",
    payload: {
      answer: { text: "not a string" },
      availability: "available",
      citations: [],
      chunks: [],
      grounding: "grounded",
    },
    role: "heading",
  },
  {
    expected: "Something went wrong",
    name: "rejects duplicate citations",
    payload: {
      answer: "A grounded answer.",
      availability: "available",
      citations: ["chunk-1", "chunk-1"],
      chunks: [{ chunkId: "chunk-1", score: 0.9, text: "Evidence" }],
      grounding: "grounded",
    },
    role: "heading",
  },
  {
    expected: "Something went wrong",
    name: "rejects a citation for an unknown chunk id",
    payload: {
      answer: "A grounded answer.",
      availability: "available",
      citations: ["unknown-chunk"],
      chunks: [{ chunkId: "chunk-1", score: 0.9, text: "Evidence" }],
      grounding: "grounded",
    },
    role: "heading",
  },
  {
    expected: "Something went wrong",
    name: "rejects citations with no returned evidence",
    payload: {
      answer: "A grounded answer.",
      availability: "available",
      citations: ["chunk-1"],
      chunks: [],
      grounding: "grounded",
    },
    role: "heading",
  },
] as const;

for (const queryContractCase of queryContractCases) {
  test(`query contract: ${queryContractCase.name}`, async ({ page }) => {
    await page.route("**/api/query", (route) =>
      route.fulfill({
        body: JSON.stringify(queryContractCase.payload),
        contentType: "application/json",
        status: 200,
      }),
    );
    await page.goto("/query");
    await page.getByRole("textbox", { name: "Ask your knowledge base" }).fill("contract case");
    await page.getByRole("button", { exact: true, name: "Ask" }).click();

    const result =
      queryContractCase.role === "heading"
        ? page.getByRole("heading", { exact: true, name: queryContractCase.expected })
        : page.getByText(queryContractCase.expected, { exact: true });
    await expect(result).toBeVisible();
  });
}
