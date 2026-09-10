import { NavLink, Outlet } from "react-router-dom";
import { Icon, type IconName } from "../ui/Icon";
import { StatusBadge } from "../ui/StatusBadge";

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
  return (
    <div className="app-shell">
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
          <StatusBadge tone="healthy">Local · Healthy</StatusBadge>
          <button aria-label="Open user menu" className="avatar" type="button">
            U
          </button>
        </header>
        <main className="content">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
