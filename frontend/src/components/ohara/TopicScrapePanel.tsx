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

const DEFAULT_TOPIC_LIMIT = 5;
const MAX_TOPIC_LIMIT = 10;
const MIN_TOPIC_LIMIT = 1;

export function TopicScrapePanel({ onQueued }: TopicScrapePanelProps) {
  const api = useApi();
  const { notify } = useNotifications();
  const [topic, setTopic] = useState("september 2026 news");
  const [limit, setLimit] = useState(String(DEFAULT_TOPIC_LIMIT));
  const [result, setResult] = useState<TopicScrapeResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [limitError, setLimitError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const normalizedTopic = topic.trim();
    setError(null);
    setLimitError(null);
    if (normalizedTopic.length === 0) {
      setError("Enter a topic to search.");
      return;
    }
    const requestedLimit = Number(limit);
    if (
      !Number.isInteger(requestedLimit) ||
      requestedLimit < MIN_TOPIC_LIMIT ||
      requestedLimit > MAX_TOPIC_LIMIT
    ) {
      setLimitError(`Choose between ${MIN_TOPIC_LIMIT} and ${MAX_TOPIC_LIMIT} articles.`);
      return;
    }
    setSubmitting(true);
    try {
      const response = await api.scrapeTopic(normalizedTopic, requestedLimit);
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
        Find recent news and queue the article pages for the local ingestion stages to process.
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
        <div className="topic-form__limit">
          <Input
            aria-describedby="topic-scrape-help"
            error={limitError ?? undefined}
            label="Max articles"
            max={MAX_TOPIC_LIMIT}
            min={MIN_TOPIC_LIMIT}
            onChange={(event) => setLimit(event.target.value)}
            step={1}
            type="number"
            value={limit}
          />
        </div>
        <Button disabled={submitting} type="submit">
          {submitting ? "Searching…" : "Find & queue"}
        </Button>
      </form>
      <p className="panel-note" id="topic-scrape-help">
        Search between {MIN_TOPIC_LIMIT} and {MAX_TOPIC_LIMIT} articles per request. The scraper,
        cleaning, and indexer processes must be running to fetch and index them.
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
