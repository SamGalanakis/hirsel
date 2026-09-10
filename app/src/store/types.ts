import type { HelloOkMsg, ModelSnapshot, ProcessInfo, PromptSnapshot, ProviderRoster, SubagentModelCatalog, TurnEvent, ViewInstance, ViewUpsertMsg } from "../protocol";
export type ConnectionStatus = "connecting" | "connected" | "reconnecting";
export interface TimelineEvent { seq: number; event: TurnEvent; at?: number }
/** Operational state only. Threads own every message, turn and activity. */
export interface AppState {
  connection: ConnectionStatus;
  hostVersion: string | null;
  model: ModelSnapshot | null;
  subagentModels: SubagentModelCatalog | null;
  prompts: PromptSnapshot | null;
  providers: ProviderRoster | null;
  processes: ProcessInfo[];
  views: ViewInstance[];
}
export type Action =
  | { type: "hello_ok"; payload: HelloOkMsg }
  | { type: "connection_status"; status: ConnectionStatus }
  | { type: "process_upsert"; payload: { type: "process_upsert"; process: ProcessInfo } }
  | { type: "view_upsert"; payload: ViewUpsertMsg }
  | { type: "view_removed"; payload: { type: "view_removed"; instance_id: string } }
  | { type: "model_changed"; model: ModelSnapshot }
  | { type: "subagent_models_changed"; catalog: SubagentModelCatalog }
  | { type: "prompts_changed"; prompts: PromptSnapshot }
  | { type: "providers_changed"; roster: ProviderRoster };
export function initialState(): AppState { return { connection: "connecting", hostVersion: null, model: null, subagentModels: null, prompts: null, providers: null, processes: [], views: [] }; }
