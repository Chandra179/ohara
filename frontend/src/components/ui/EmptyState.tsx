import { Icon, type IconName } from "./Icon";

interface EmptyStateProps {
  description: string;
  icon?: IconName;
  title: string;
}

export function EmptyState({
  description,
  icon = "activity",
  title,
}: EmptyStateProps) {
  return (
    <div className="empty-state">
      <span aria-hidden="true" className="empty-state__icon">
        <Icon name={icon} />
      </span>
      <h2>{title}</h2>
      <p>{description}</p>
    </div>
  );
}
