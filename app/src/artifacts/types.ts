/** UI-only signal from an opaque preview; never a host command. */
export const ARTIFACT_DISMISS_MESSAGE = "hirsel:artifact-dismiss";

/** The one render discriminator, mirroring `hirsel_proto::ArtifactKind`. It
 * travels flat beside the rest of the summary, and each variant carries only
 * the data its own surface needs: no second field can disagree about how a
 * result opens. */
export type ArtifactKind =
  | { kind: "solid" }
  | { kind: "html" }
  | { kind: "markdown" }
  | { kind: "openui" }
  | { kind: "image"; mime: string }
  | { kind: "file"; mime: string; filename?: string | null };
export type ArtifactSummary = ArtifactKind & {
  id: number;
  title: string;
  created_at: string;
  updated_at: string;
  thread_ids: number[];
};
export type Artifact = ArtifactSummary & { content: string };
export type ArtifactClientMessage =
  | { type: "list_artifacts"; client_id: string; thread_id?: number }
  | { type: "open_artifact"; client_id: string; artifact_id: number };
export type ArtifactServerMessage =
  | { type: "artifacts_listed"; client_id: string; artifacts: ArtifactSummary[] }
  | { type: "artifact_opened"; client_id: string; artifact: Artifact }
  | { type: "artifact_upsert"; artifact: ArtifactSummary };
