import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ErrorState } from "./ui/ErrorState";
import { Input } from "./ui/Input";
import { Modal } from "./ui/Modal";
import { Table } from "./ui/Table";

describe("shared UI primitives", () => {
  it("associates an input with its label and validation message", () => {
    render(<Input aria-describedby="document-name-help" error="A name is required" id="document-name" label="Document name" />);

    expect(screen.getByRole("textbox", { name: "Document name" })).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    expect(screen.getByRole("textbox", { name: "Document name" })).toHaveAttribute(
      "aria-describedby",
      "document-name-help document-name-error",
    );
    expect(screen.getByRole("alert")).toHaveTextContent("A name is required");
  });

  it("renders typed table rows and headers", () => {
    render(
      <Table
        caption="Documents"
        columns={[
          { header: "Name", key: "name", render: (row) => row.name },
          { header: "Count", key: "count", render: (row) => row.count },
        ]}
        getRowKey={(row) => row.name}
        rows={[{ count: 3, name: "Notes.md" }]}
      />,
    );

    expect(screen.getByRole("columnheader", { name: "Name" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "Notes.md" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "3" })).toBeInTheDocument();
  });

  it("supports closing a modal with keyboard and retrying an error", () => {
    const onClose = vi.fn();
    const onRetry = vi.fn();

    render(
      <>
        <Modal onClose={onClose} title="Confirm merge">
          Review the selected entities.
        </Modal>
        <ErrorState description="The request failed." onRetry={onRetry} />
      </>,
    );

    fireEvent.keyDown(screen.getByRole("button", { name: "Close dialog" }), { key: "Escape" });
    fireEvent.click(screen.getByRole("button", { name: "Close dialog" }));
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));

    expect(onClose).toHaveBeenCalledTimes(2);
    expect(onRetry).toHaveBeenCalledOnce();
  });
});
