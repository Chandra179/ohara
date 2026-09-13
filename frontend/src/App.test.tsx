import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { BrowserRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { createMockApi, type OharaApi } from "./api/client";

function renderApp(api?: OharaApi) {
  return render(
    <BrowserRouter>
      <App api={api} />
    </BrowserRouter>,
  );
}

describe("App shell", () => {
  it("renders the overview and shared navigation", async () => {
    renderApp();

    expect(screen.getByRole("heading", { name: "Overview" })).toBeInTheDocument();
    expect(
      screen.getByRole("complementary", { name: "Primary navigation" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Skip to content" })).toHaveAttribute(
      "href",
      "#main-content",
    );
    expect(await screen.findByRole("heading", { name: "Ingestion queue" })).toBeInTheDocument();
    expect(screen.getAllByText("Local · Healthy")).toHaveLength(2);
    expect(screen.getByText("Pipeline · Ready")).toBeInTheDocument();
    expect(screen.getByText("Product Notes Q1")).toBeInTheDocument();
  });

  it("refreshes ingestion status and announces the result", async () => {
    const user = userEvent.setup();
    renderApp();

    await user.click(await screen.findByRole("button", { name: "Refresh" }));

    expect(await screen.findByText("Ingestion status refreshed")).toBeInTheDocument();
  });

  it("finds and queues a topic from the overview", async () => {
    const user = userEvent.setup();
    renderApp();

    const maxArticles = await screen.findByRole("spinbutton", { name: "Max articles" });
    expect(maxArticles).toHaveValue(5);
    await user.clear(maxArticles);
    await user.type(maxArticles, "2");
    await user.click(await screen.findByRole("button", { name: "Find & queue" }));

    expect(await screen.findByText(/2 results found for/)).toBeInTheDocument();
    expect(screen.getByText("2 queued · 0 already in your library.")).toBeInTheDocument();
    expect(await screen.findByText("Topic queued")).toBeInTheDocument();
  });

  it("moves focus to the main content after navigation", async () => {
    const user = userEvent.setup();
    renderApp();

    await user.click(screen.getByRole("link", { name: "Documents" }));

    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("main"));
    });
  });

  it("navigates between route boundaries", async () => {
    const user = userEvent.setup();
    renderApp();

    await user.click(screen.getByRole("link", { name: "Documents" }));

    expect(await screen.findByRole("heading", { name: "Documents" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "8 documents" })).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Search documents" })).toBeInTheDocument();

    await user.selectOptions(screen.getByRole("combobox", { name: "Status" }), "NEW");
    expect(await screen.findByRole("heading", { name: "1 document" })).toBeInTheDocument();
    expect(screen.getByText("Product Notes Q1")).toBeInTheDocument();
  });

  it("does not fetch documents while search text is being edited", async () => {
    const user = userEvent.setup();
    const api = createMockApi();
    const listDocuments = vi.spyOn(api, "listDocuments");
    renderApp(api);

    await user.click(screen.getByRole("link", { name: "Documents" }));
    await screen.findByRole("heading", { name: "8 documents" });
    listDocuments.mockClear();

    const search = screen.getByRole("textbox", { name: "Search documents" });
    await user.type(search, "research");

    expect(listDocuments).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Search" }));

    expect(await screen.findByRole("heading", { name: "1 document" })).toBeInTheDocument();
    expect(listDocuments).toHaveBeenCalledOnce();
    expect(listDocuments).toHaveBeenCalledWith({
      cursor: undefined,
      limit: 25,
      search: "research",
      status: undefined,
    });
  });

  it("renders the Operations metrics snapshot", async () => {
    const user = userEvent.setup();
    renderApp();

    await user.click(screen.getByRole("link", { name: "Operations" }));

    expect(await screen.findByRole("heading", { name: "Pipeline activity" })).toBeInTheDocument();
    expect(screen.getByText("Active queue")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "LLM usage" })).toBeInTheDocument();
    expect(screen.getByText("Throughput and latency are not available yet")).toBeInTheDocument();
  });

  it("runs a grounded query and renders citations", async () => {
    const user = userEvent.setup();
    renderApp();

    await user.click(screen.getByRole("link", { name: "Query" }));
    expect(await screen.findByText("Start with a question")).toBeInTheDocument();
    await user.type(
      await screen.findByRole("textbox", { name: "Ask your knowledge base" }),
      "What is Ohara?",
    );
    await user.click(screen.getByRole("button", { name: "Ask" }));

    expect(await screen.findByRole("heading", { name: "Answer" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Citations" })).toBeInTheDocument();
    expect(screen.getByText("ohara-product-guide.pdf")).toBeInTheDocument();
  });

  it("shows the ungrounded query state", async () => {
    const user = userEvent.setup();
    renderApp();

    await user.click(screen.getByRole("link", { name: "Query" }));
    await user.type(
      await screen.findByRole("textbox", { name: "Ask your knowledge base" }),
      "Show an ungrounded answer",
    );
    await user.click(screen.getByRole("button", { name: "Ask" }));

    expect(await screen.findByText("No grounded answer")).toBeInTheDocument();
  });

  it("shows the unavailable model state", async () => {
    const user = userEvent.setup();
    renderApp();

    await user.click(screen.getByRole("link", { name: "Query" }));
    await user.type(
      await screen.findByRole("textbox", { name: "Ask your knowledge base" }),
      "Use the offline provider",
    );
    await user.click(screen.getByRole("button", { name: "Ask" }));

    expect(await screen.findByText("Local model unavailable")).toBeInTheDocument();
  });

  it("loads an entity review preview", async () => {
    const user = userEvent.setup();
    renderApp();

    await user.click(screen.getByRole("link", { name: "Entities" }));
    await user.click(await screen.findByRole("button", { name: /Ohara.*Ohara/ }));

    expect(await screen.findByRole("heading", { name: "Candidate preview" })).toBeInTheDocument();
    expect(screen.getAllByText("94.0% match")).toHaveLength(2);
  });
});
