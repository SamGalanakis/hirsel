import type { Action, AppState } from "./types";
export function reduce(state: AppState, action: Action): AppState {
  switch (action.type) {
    case "hello_ok": {
      const hello = action.payload;
      return { ...state, hostVersion: hello.host_version, model: hello.model, subagentModels: hello.subagent_models, prompts: hello.prompts, providers: hello.providers, processes: hello.processes, views: hello.views };
    }
    case "connection_status": return { ...state, connection: action.status };
    case "process_upsert": return { ...state, processes: [...state.processes.filter(row => row.id !== action.payload.process.id), action.payload.process] };
    case "view_upsert": {
      const { instance_id, thread_id, placement, spec } = action.payload;
      return { ...state, views: [...state.views.filter(row => row.instance_id !== instance_id), { instance_id, thread_id, placement, spec }] };
    }
    case "view_removed": return { ...state, views: state.views.filter(row => row.instance_id !== action.payload.instance_id) };
    case "model_changed": return { ...state, model: action.model };
    case "subagent_models_changed": return { ...state, subagentModels: action.catalog };
    case "prompts_changed": return { ...state, prompts: action.prompts };
    case "providers_changed": return { ...state, providers: action.roster };
  }
}
