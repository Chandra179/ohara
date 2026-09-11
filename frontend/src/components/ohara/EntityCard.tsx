import type { EntityRecord } from "../../api/client";

interface EntityCardProps {
  entity: EntityRecord;
  label: string;
}

export function EntityCard({ entity, label }: EntityCardProps) {
  return (
    <div className="entity-card">
      <span className="entity-card__label">{label}</span>
      <strong>{entity.name}</strong>
      <span>{entity.type.toLowerCase()}</span>
      <small>{entity.references} references</small>
    </div>
  );
}
