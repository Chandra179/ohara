import { useEffect, useId, useRef, type ReactNode } from "react";
import { Button } from "./Button";

interface ModalProps {
  children: ReactNode;
  onClose: () => void;
  title: string;
}

export function Modal({ children, onClose, title }: ModalProps) {
  const titleId = useId();
  const dialogRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    const dialog = dialogRef.current;
    const previouslyFocused = document.activeElement as HTMLElement | null;

    if (!dialog) {
      return undefined;
    }

    const focusTrap = dialog;

    const getFocusableElements = () =>
      Array.from(
        focusTrap.querySelectorAll<HTMLElement>(
          'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
        ),
      );

    getFocusableElements()[0]?.focus();

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        onCloseRef.current();
        return;
      }

      if (event.key !== "Tab") {
        return;
      }

      const focusableElements = getFocusableElements();
      if (focusableElements.length === 0) {
        event.preventDefault();
        focusTrap.focus();
        return;
      }

      const first = focusableElements[0];
      const last = focusableElements[focusableElements.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }

    focusTrap.addEventListener("keydown", handleKeyDown);
    return () => {
      focusTrap.removeEventListener("keydown", handleKeyDown);
      previouslyFocused?.focus();
    };
  }, []);

  return (
    <div aria-labelledby={titleId} aria-modal="true" className="modal-backdrop" role="dialog">
      <div className="modal" ref={dialogRef} tabIndex={-1}>
        <div className="modal__heading">
          <h2 id={titleId}>{title}</h2>
          <Button aria-label="Close dialog" onClick={onClose} variant="ghost">
            Close
          </Button>
        </div>
        <div className="modal__body">{children}</div>
      </div>
    </div>
  );
}
