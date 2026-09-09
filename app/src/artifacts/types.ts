/** UI-only signal from an opaque preview; never a host command. */
export const ARTIFACT_DISMISS_MESSAGE = "hirsel:artifact-dismiss";

export interface ArtifactSummary {
  id: number;
  title: string;
  kind: "solid" | "html" | "file";
  mime: string;
  filename?: string | null;
  created_at: string;
  updated_at: string;
  thread_ids: number[];
}
export interface Artifact extends ArtifactSummary { content: string }
export type ArtifactClientMessage =
  | { type: "list_artifacts"; client_id: string; thread_id?: number }
  | { type: "open_artifact"; client_id: string; artifact_id: number };
export type ArtifactServerMessage =
  | { type: "artifacts_listed"; client_id: string; artifacts: ArtifactSummary[] }
  | { type: "artifact_opened"; client_id: string; artifact: Artifact }
  | { type: "artifact_upsert"; artifact: ArtifactSummary };
