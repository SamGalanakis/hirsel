// Settings → Device label & identity: the local display name for this browser
// and the one-way fingerprint of its access token. Both are browser-local — the
// web client sends neither to the Host.
import { createSignal, type JSX } from "solid-js";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { Card, CopyRow, Field, SectionHeader } from "./rows";

export function IdentitySection(props: {
  deviceLabel: string;
  fingerprint: string;
  /** Persist a trimmed, non-empty, changed label (the panel owns the store). */
  onSaveLabel: (label: string) => void;
}): JSX.Element {
  const [labelDraft, setLabelDraft] = createSignal(props.deviceLabel);

  const canSaveLabel = () => {
    const t = labelDraft().trim();
    return t.length > 0 && t !== props.deviceLabel;
  };

  function saveLabel() {
    const trimmed = labelDraft().trim();
    if (trimmed.length === 0 || trimmed === props.deviceLabel) return;
    setLabelDraft(trimmed);
    props.onSaveLabel(trimmed);
  }

  return (
    <>
      <SectionHeader>Device label &amp; identity</SectionHeader>
      <Card class="p-3.5">
        <Field
          title="Device label"
          subtitle="A local name for this browser. Stored here only — the web client sends no label to the Host."
        />
        <div class="mt-2.5 flex items-center gap-2">
          <Input
            value={labelDraft()}
            onInput={(e) => setLabelDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") saveLabel();
            }}
            placeholder="This browser"
            class="h-9 text-sm"
            aria-label="Device label"
          />
          <Button size="sm" class="h-9 shrink-0" disabled={!canSaveLabel()} onClick={saveLabel}>
            Save
          </Button>
        </div>
      </Card>
      <Card class="mt-2.5 p-3.5">
        <Field
          title="Device identity"
          subtitle="A stable, one-way fingerprint of this browser's access token. Safe to share."
        />
        <div class="mt-2.5">
          <CopyRow value={props.fingerprint} label="identity fingerprint" mono />
        </div>
      </Card>
    </>
  );
}
