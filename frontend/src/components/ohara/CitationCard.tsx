import type { Citation } from "../../api/client";
import { Icon } from "../ui/Icon";

export function CitationCard({ citation }: { citation: Citation }) {
  return (
    <article className="citation-card">
      <div className="citation-card__heading">
        <Icon name="file" />
        <strong>{citation.title}</strong>
      </div>
      <span>{citation.source}</span>
      <small>{citation.location}</small>
    </article>
  );
}
