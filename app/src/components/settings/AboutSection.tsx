// Settings → About & debug: the app/host versions, the local "Show agent code"
// preference, and the copyable diagnostics blob.
import { Copy } from "@/components/ui/icons";
import type { JSX } from "@solidjs/web";
import { setShowAgentCode, showAgentCode } from "../../lib/prefs";
import { APP_VERSION } from "../../lib/version";
import { state } from "../../store/store";
import { Group, Field, Toggle } from "./rows";

export function AboutSection(props: {
  onCopyDiagnostics: () => void;
}): JSX.Element {
  return (
    <>
      <Group class="divide-y divide-border">
        <div class="flex items-center justify-between gap-3 py-3">
          <span class="text-sm text-foreground">App version</span>
          <span class="font-mono text-xs text-muted-foreground">{APP_VERSION}</span>
        </div>
        <div class="flex items-center justify-between gap-3 py-3">
          <span class="text-sm text-foreground">Host version</span>
          <span class="font-mono text-xs text-muted-foreground">
            {state.hostVersion ?? (state.connection === "connected" ? "Not reported" : "—")}
          </span>
        </div>
        <div class="flex items-center gap-3 py-3">
          <Field
            title="Show agent code"
            subtitle="Render the Agent's own program for each turn step in the timeline."
          />
          <Toggle
            ariaLabel="Show agent code"
            checked={showAgentCode()}
            onChange={setShowAgentCode}
          />
        </div>
        <button
          type="button"
          onClick={props.onCopyDiagnostics}
          class="flex w-full items-center gap-3 py-3 text-left outline-none transition-colors hover:bg-muted focus-visible:bg-muted"
        >
          <div class="min-w-0 flex-1">
            <Field
              title="Copy diagnostics"
              subtitle="Version, connection, and settings — no secrets."
            />
          </div>
          <Copy class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
        </button>
      </Group>
    </>
  );
}
