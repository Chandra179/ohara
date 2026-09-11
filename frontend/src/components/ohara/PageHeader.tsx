import type { ReactNode } from "react";

interface PageHeaderProps {
  actions?: ReactNode;
  description: string;
  label: string;
}

export function PageHeader({ actions, description, label }: PageHeaderProps) {
  return (
    <div className="page-heading">
      <div className="page-heading__bar">
        <div>
          <p className="eyebrow">Local knowledge workbench</p>
          <h1 id="page-title">{label}</h1>
          <p>{description}</p>
        </div>
        {actions ? <div className="page-heading__actions">{actions}</div> : null}
      </div>
    </div>
  );
}
