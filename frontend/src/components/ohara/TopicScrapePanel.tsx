import { type FormEvent, useState } from "react";
import type { TopicScrapeResult } from "../../api/client";
import { useApi } from "../../api/useApi";
import { useNotifications } from "../../notifications/useNotifications";
import { Button } from "../ui/Button";
import { Input } from "../ui/Input";
import { Panel } from "../ui/Panel";

interface TopicScrapePanelProps {
  onQueued: () => void;
}

export function TopicScrapePanel({ onQueued }: TopicScrapePanelProps) {
  const api = useApi();
  const { notify } = useNotifications();
  const [topic, setTopic] = useState("september 2026 news");
  const [result, setResult] = useState<TopicScrapeResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const normalizedTopic = topic.trim();
    if (normalizedTopic.length === 0) {
      setError("Enter a topic to search.");
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      const response = await api.scrapeTopic(normalizedTopic);
      setResult(response);
      onQueued();
      notify({
        message: `${response.enqueued} article${response.enqueued === 1 ? "" : "s"} added to the ingestion queue.`,
        title: "Topic queued",
        tone: "success",
      });
    } catch (requestError: unknown) {
      const message = requestError instanceof Error ? requestError.message : "Topic search failed";
      setError(message);
      setResult(null);
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <Panel>
      <div className="panel-heading">
        <div>
          <p className="eyebrow">Discover</p>
          <h2>Scrape a topic</h2>
        </div>
      </div>
      <p className="panel-copy">
        Find recent news and queue the article pages for the local worker to process.
      </p>
      <form className="topic-form" onSubmit={submit}>
        <Input
          aria-describedby="topic-scrape-help"
          error={error ?? undefined}
          label="Topic"
          onChange={(event) => setTopic(event.target.value)}
          placeholder="september 2026 news"
          value={topic}
        />
        <Button disabled={submitting} type="submit">
          {submitting ? "Searching…" : "Find & queue"}
        </Button>
      </form>
      <p className="panel-note" id="topic-scrape-help">
        Up to five results are searched per request. The worker must be running to fetch and index them.
      </p>
      {result ? (
        <div className="notice notice--success topic-result" role="status">
          <strong>{result.discovered} result{result.discovered === 1 ? "" : "s"} found for “{result.topic}”</strong>
          <span>{result.enqueued} queued · {result.duplicates} already in your library.</span>
        </div>
      ) : null}
    </Panel>
  );
}
