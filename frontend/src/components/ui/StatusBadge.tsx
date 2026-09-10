import type { HTMLAttributes } from "react";

export type StatusTone = "danger" | "healthy" | "muted" | "pending";

interface StatusBadgeProps extends HTMLAttributes<HTMLSpanElement> {
  tone: StatusTone;
}

export function StatusBadge({
  children,
  className = "",
  tone,
  ...props
}: StatusBadgeProps) {
  return (
    <span className={`status-badge status-badge--${tone} ${className}`.trim()} {...props}>
      <span aria-hidden="true" className="status-badge__dot" />
      {children}
    </span>
  );
}
