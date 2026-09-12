import { useCallback, useEffect, useRef } from "react";
import { NavLink, Outlet, useLocation } from "react-router-dom";
import type {
  HealthSnapshot,
  ReadinessDiagnostic,
  ServiceStatus,
  WorkerSnapshot,
  WorkerState,
} from "../../api/client";
import { useApi } from "../../api/useApi";
import { Icon, type IconName } from "../ui/Icon";
import { StatusBadge, type StatusTone } from "../ui/StatusBadge";
import { useAsyncResource, type AsyncResource } from "../../hooks/useAsyncResource";

interface NavigationItem {
  icon: IconName;
  label: string;
  to: string;
}

const primaryNavigation: NavigationItem[] = [
  { icon: "home", label: "Overview", to: "/" },
  { icon: "file", label: "Documents", to: "/documents" },
  { icon: "search", label: "Query", to: "/query" },
  { icon: "nodes", label: "Entities", to: "/entities" },
];

const secondaryNavigation: NavigationItem[] = [
  { icon: "flask", label: "Quality Lab", to: "/quality" },
  { icon: "activity", label: "Operations", to: "/operations" },
];

const SERVICE_LABELS: Record<ServiceStatus, string> = {
  degraded: "Local · Degraded",
  healthy: "Local · Healthy",
  offline: "Local · Offline",
};

const SERVICE_TONES: Record<ServiceStatus, StatusTone> = {
  degraded: "pending",
  healthy: "healthy",
  offline: "danger",
};

function NavigationLink({ icon, label, to }: NavigationItem) {
  return (
    <NavLink
      className={({ isActive }) => `nav-link${isActive ? " nav-link--active" : ""}`}
      end={to === "/"}
      to={to}
    >
      <Icon name={icon} />
      <span>{label}</span>
    </NavLink>
  );
}

export function AppShell() {
  const api = useApi();
  const location = useLocation();
  const loadHealth = useCallback(() => api.getHealth(), [api]);
  const { resource: health } = useAsyncResource<HealthSnapshot>(loadHealth);
  const mainRef = useRef<HTMLElement>(null);
  const previousPathname = useRef(location.pathname);

  useEffect(() => {
    if (previousPathname.current === location.pathname) {
      return;
    }

    previousPathname.current = location.pathname;
    mainRef.current?.focus();
  }, [location.pathname]);

  return (
    <div className="app-shell">
      <a className="skip-link" href="#main-content">
        Skip to content
      </a>
      <aside aria-label="Primary navigation" className="sidebar">
        <div className="brand">OHARA</div>

        <nav className="nav-group" aria-label="Primary">
          {primaryNavigation.map((item) => (
            <NavigationLink key={item.to} {...item} />
          ))}
        </nav>

        <nav className="nav-group nav-group--secondary" aria-label="Secondary">
          {secondaryNavigation.map((item) => (
            <NavigationLink key={item.to} {...item} />
          ))}
        </nav>

        <div className="sidebar-footer">
          <NavigationLink icon="settings" label="Settings" to="/settings" />
        </div>
      </aside>

      <div className="app-main">
        <header className="top-bar">
          <div className="status-cluster">
            <HealthBadge health={health} />
            <WorkerBadge health={health} />
          </div>
          <button aria-label="Open user menu" className="avatar" type="button">
            U
          </button>
        </header>
        <main
          aria-labelledby="page-title"
          className="content"
          id="main-content"
          ref={mainRef}
          tabIndex={-1}
        >
          <ReadinessNotice health={health} />
          <Outlet />
        </main>
      </div>
    </div>
  );
}

function WorkerBadge({ health }: { health: AsyncResource<HealthSnapshot> }) {
  if (health.status === "loading") {
    return <StatusBadge tone="muted">Worker · Checking…</StatusBadge>;
  }

  if (health.status === "error") {
    return <StatusBadge title={health.error} tone="danger">Worker · Unknown</StatusBadge>;
  }

  const worker = health.data.worker;
  const label = workerLabel(worker);
  const tone: StatusTone = worker.status === "available"
    ? "healthy"
    : worker.state === "starting" || worker.state === "stopping"
    ? "pending"
    : "danger";
  const title = worker.lastError ?? (worker.stale ? "The worker heartbeat is stale." : undefined);
  return <StatusBadge title={title} tone={tone}>{label}</StatusBadge>;
}

function workerLabel(worker: WorkerSnapshot): string {
  if (worker.state === null) {
    return "Worker · Not running";
  }
  return `Worker · ${formatWorkerState(worker.state)}`;
}

function formatWorkerState(state: WorkerState): string {
  return state.charAt(0).toUpperCase() + state.slice(1);
}

function HealthBadge({ health }: { health: AsyncResource<HealthSnapshot> }) {
  if (health.status === "loading") {
    return <StatusBadge tone="muted">Local · Checking…</StatusBadge>;
  }

  if (health.status === "error") {
    return <StatusBadge title={health.error} tone="danger">Local · Unavailable</StatusBadge>;
  }

  return (
    <StatusBadge
      title={health.data.diagnostics.map((diagnostic) => `${diagnostic.message} ${diagnostic.action}`).join(" ")}
      tone={SERVICE_TONES[health.data.status]}
    >
      {SERVICE_LABELS[health.data.status]}
    </StatusBadge>
  );
}

function ReadinessNotice({ health }: { health: AsyncResource<HealthSnapshot> }) {
  if (health.status === "error") {
    return (
      <div className="notice notice--error readiness-notice" role="alert">
        <strong>Local API unavailable</strong>
        <span>{health.error}. Start the Rust API and try again.</span>
      </div>
    );
  }

  if (health.status !== "success" || health.data.diagnostics.length === 0) {
    return null;
  }

  return (
    <div className="notice notice--error readiness-notice" role="status">
      <strong>Local readiness needs attention</strong>
      <ul>
        {health.data.diagnostics.map((diagnostic) => (
          <ReadinessItem diagnostic={diagnostic} key={`${diagnostic.component}-${diagnostic.message}`} />
        ))}
      </ul>
    </div>
  );
}

function ReadinessItem({ diagnostic }: { diagnostic: ReadinessDiagnostic }) {
  return (
    <li>
      <strong>{formatComponent(diagnostic.component)}</strong>
      <span>
        {diagnostic.message} {diagnostic.action}
      </span>
    </li>
  );
}

function formatComponent(component: ReadinessDiagnostic["component"]): string {
  return component.replace(/([A-Z])/g, " $1").toLowerCase();
}
