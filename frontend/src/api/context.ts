import { createContext } from "react";
import type { OharaApi } from "./client";

export const ApiContext = createContext<OharaApi | undefined>(undefined);
