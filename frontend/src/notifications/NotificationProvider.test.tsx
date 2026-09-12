import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { NotificationProvider } from "./NotificationProvider";
import { useNotifications } from "./useNotifications";

function NotificationTrigger() {
  const { notify } = useNotifications();

  return (
    <button
      onClick={() => notify({ message: "Saved locally.", title: "Saved", tone: "success" })}
      type="button"
    >
      Notify
    </button>
  );
}

describe("NotificationProvider", () => {
  it("automatically dismisses notifications after ten seconds", () => {
    vi.useFakeTimers();

    try {
      render(
        <NotificationProvider>
          <NotificationTrigger />
        </NotificationProvider>,
      );

      fireEvent.click(screen.getByRole("button", { name: "Notify" }));
      expect(screen.getByRole("status")).toHaveTextContent("Saved");

      act(() => vi.advanceTimersByTime(9_999));
      expect(screen.getByRole("status")).toHaveTextContent("Saved");

      act(() => vi.advanceTimersByTime(1));
      expect(screen.queryByRole("status")).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });
});
