import { useId, type ReactNode } from "react";
import { Button } from "./Button";

interface ModalProps {
  children: ReactNode;
  onClose: () => void;
  title: string;
}

export function Modal({ children, onClose, title }: ModalProps) {
  const titleId = useId();

  return (
    <div aria-labelledby={titleId} aria-modal="true" className="modal-backdrop" role="dialog">
      <div className="modal">
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
