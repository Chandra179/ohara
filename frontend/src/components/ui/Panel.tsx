import type { HTMLAttributes } from "react";

interface PanelProps extends HTMLAttributes<HTMLElement> {
  as?: "div" | "section";
}

export function Panel({ as = "section", className = "", ...props }: PanelProps) {
  const Element = as;

  return <Element className={`panel ${className}`.trim()} {...props} />;
}
