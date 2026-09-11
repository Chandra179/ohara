import { Navigate, Route, Routes } from "react-router-dom";
import { ApiProvider } from "./api/ApiProvider";
import { AppShell } from "./components/layout/AppShell";
import { FoundationPage, type PageKey } from "./pages/FoundationPage";
import { DocumentsPage } from "./pages/DocumentsPage";
import { EntitiesPage } from "./pages/EntitiesPage";
import { OverviewPage } from "./pages/OverviewPage";
import { QueryPage } from "./pages/QueryPage";
import { OperationsPage } from "./pages/OperationsPage";
import type { OharaApi } from "./api/client";
import { NotificationProvider } from "./notifications/NotificationProvider";

interface PageRoute {
  path: string;
  page: PageKey;
}

const FOUNDATION_ROUTES: PageRoute[] = [
  { page: "quality", path: "quality" },
  { page: "settings", path: "settings" },
];

interface AppProps {
  api?: OharaApi;
}

export function App({ api }: AppProps) {
  return (
    <ApiProvider api={api}>
      <NotificationProvider>
        <Routes>
          <Route element={<AppShell />}>
            <Route element={<OverviewPage />} index />
            <Route element={<DocumentsPage />} path="documents" />
            <Route element={<QueryPage />} path="query" />
            <Route element={<EntitiesPage />} path="entities" />
            <Route element={<OperationsPage />} path="operations" />
            {FOUNDATION_ROUTES.map(({ page, path }) => (
              <Route element={<FoundationPage page={page} />} key={path} path={path} />
            ))}
            <Route element={<Navigate replace to="/" />} path="*" />
          </Route>
        </Routes>
      </NotificationProvider>
    </ApiProvider>
  );
}
