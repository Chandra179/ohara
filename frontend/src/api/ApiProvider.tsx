import { useState, type ReactNode } from "react";
import { ApiContext } from "./context";
import { createMockApi, type OharaApi } from "./client";
import { createHttpApi } from "./http";

interface ApiProviderProps {
  api?: OharaApi;
  children: ReactNode;
}

export function ApiProvider({ api, children }: ApiProviderProps) {
  const [defaultApi] = useState<OharaApi>(() => {
    if (import.meta.env.VITE_OHARA_API_MODE === "http") {
      return createHttpApi({ baseUrl: import.meta.env.VITE_OHARA_API_BASE_URL });
    }
    return createMockApi();
  });

  return <ApiContext.Provider value={api ?? defaultApi}>{children}</ApiContext.Provider>;
}
