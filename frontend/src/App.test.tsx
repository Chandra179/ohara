import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { BrowserRouter } from "react-router-dom";
import { describe, expect, it } from "vitest";
import { App } from "./App";

function renderApp() {
  return render(
    <BrowserRouter>
      <App />
    </BrowserRouter>,
  );
}

describe("App shell", () => {
  it("renders the overview and shared navigation", () => {
    renderApp();

    expect(screen.getByRole("heading", { name: "Overview" })).toBeInTheDocument();
    expect(
      screen.getByRole("complementary", { name: "Primary navigation" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Local · Healthy")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Implementation plan" }),
    ).not.toBeInTheDocument();
  });

  it("navigates between route boundaries", async () => {
    const user = userEvent.setup();
    renderApp();

    await user.click(screen.getByRole("link", { name: "Documents" }));

    expect(screen.getByRole("heading", { name: "Documents" })).toBeInTheDocument();
    expect(screen.getByText("Primary workflow coming next")).toBeInTheDocument();
  });
});
