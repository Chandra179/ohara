import { Icon, type IconName } from "../ui/Icon";

interface StatCardProps {
  caption?: string;
  icon: IconName;
  label: string;
  value: number | string;
}

export function StatCard({ caption = "documents", icon, label, value }: StatCardProps) {
  return (
    <div className="stat-card">
      <span aria-hidden="true" className="stat-card__icon">
        <Icon name={icon} />
      </span>
      <span className="stat-card__label">{label}</span>
      <strong>{typeof value === "number" ? value.toLocaleString() : value}</strong>
      <span className="stat-card__caption">{caption}</span>
    </div>
  );
}
