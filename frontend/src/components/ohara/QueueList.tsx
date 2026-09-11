import { Link } from "react-router-dom";
import type { QueueItem } from "../../api/client";
import { Icon } from "../ui/Icon";
import { StatusText } from "./StatusText";

interface QueueListProps {
  items: QueueItem[];
}

export function QueueList({ items }: QueueListProps) {
  if (items.length === 0) {
    return <p className="muted-copy">The ingestion queue is clear.</p>;
  }

  return (
    <div className="queue-list">
      {items.map((item) => (
        <div className="queue-row" key={item.id}>
          <Icon name="file" />
          <span className="queue-row__name">{item.name}</span>
          <StatusText status={item.status} />
          <span className="queue-row__time">{item.updatedAt}</span>
        </div>
      ))}
      <Link className="text-link" to="/documents">
        View all documents
      </Link>
    </div>
  );
}
