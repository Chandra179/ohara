import { Navigate, Route, Routes } from "react-router-dom";
import { AppShell } from "./components/layout/AppShell";
import { FoundationPage, type PageKey } from "./pages/FoundationPage";

interface PageRoute {
  path: string;
  page: PageKey;
}

const PAGE_ROUTES: PageRoute[] = [
  { page: "documents", path: "documents" },
  { page: "query", path: "query" },
  { page: "entities", path: "entities" },
  { page: "quality", path: "quality" },
  { page: "operations", path: "operations" },
  { page: "settings", path: "settings" },
];

export function App() {
  return (
    <Routes>
      <Route element={<AppShell />}>
        <Route element={<FoundationPage page="overview" />} index />
        {PAGE_ROUTES.map(({ page, path }) => (
          <Route element={<FoundationPage page={page} />} key={path} path={path} />
        ))}
        <Route element={<Navigate replace to="/" />} path="*" />
      </Route>
    </Routes>
  );
}
