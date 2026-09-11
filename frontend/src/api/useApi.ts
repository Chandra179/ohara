import { useContext } from "react";
import { ApiContext } from "./context";
import type { OharaApi } from "./client";

export function useApi(): OharaApi {
  const api = useContext(ApiContext);
  if (!api) {
    throw new Error("useApi must be used within an ApiProvider");
  }

  return api;
}
