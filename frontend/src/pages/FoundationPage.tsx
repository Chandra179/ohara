import { EmptyState } from "../components/ui/EmptyState";
import { Panel } from "../components/ui/Panel";
import { StatusBadge } from "../components/ui/StatusBadge";

export type PageKey =
  | "documents"
  | "entities"
  | "operations"
  | "overview"
  | "query"
  | "quality"
  | "settings";

interface PageDefinition {
  description: string;
  label: string;
}

const PAGE_DEFINITIONS: Record<PageKey, PageDefinition> = {
  documents: {
    description: "Browse and manage your local knowledge.",
    label: "Documents",
  },
  entities: {
    description: "Review and resolve people, places, and concepts.",
    label: "Entities",
  },
  operations: {
    description: "Inspect worker health and operational activity.",
    label: "Operations",
  },
  overview: {
    description: "Your local knowledge workbench.",
    label: "Overview",
  },
  query: {
    description: "Ask questions across your local knowledge.",
    label: "Query",
  },
  quality: {
    description: "Measure retrieval quality as the evaluation contracts mature.",
    label: "Quality Lab",
  },
  settings: {
    description: "Configure this local workspace.",
    label: "Settings",
  },
};

interface FoundationPageProps {
  page: PageKey;
}

export function FoundationPage({ page }: FoundationPageProps) {
  const definition = PAGE_DEFINITIONS[page];

  return (
    <div className="page-stack">
      <div className="page-heading">
        <p className="eyebrow">Local knowledge workbench</p>
        <h1>{definition.label}</h1>
        <p>{definition.description}</p>
      </div>

      <Panel>
        <div className="panel-heading">
          <div>
            <p className="eyebrow">P0 foundation</p>
            <h2>Workspace shell online</h2>
          </div>
          <StatusBadge tone="healthy">Ready</StatusBadge>
        </div>

        <p className="panel-copy">
          The shared layout, accessible primitives, design tokens, and typed API
          boundary are ready for the first workflow slice.
        </p>

        <div className="foundation-grid">
          <div>
            <span className="foundation-grid__label">Navigation</span>
            <strong>6 routes</strong>
          </div>
          <div>
            <span className="foundation-grid__label">Theme</span>
            <strong>Dark + teal</strong>
          </div>
          <div>
            <span className="foundation-grid__label">Data source</span>
            <strong>Typed adapter</strong>
          </div>
        </div>

        <p className="panel-note">Continue with the P0 checklist before starting screen work.</p>
      </Panel>

      <Panel>
        <EmptyState
          description="Screen-specific data and interactions are the next priority after this shared foundation."
          icon="activity"
          title="Primary workflow coming next"
        />
      </Panel>
    </div>
  );
}
