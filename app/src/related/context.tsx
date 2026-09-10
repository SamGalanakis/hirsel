import { createContext, useContext } from "solid-js";
import type { RelatedOrigin } from "./store";
export const RelatedContext = createContext<RelatedOrigin | null>(null);
export const useRelatedOrigin = () => useContext(RelatedContext);
