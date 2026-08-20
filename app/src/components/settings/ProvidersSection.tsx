// Settings → Providers: the configured roster an agent can run on. Flat rows on
// the canvas, hairlines between them — the roster is a list, not a deck of
// cards (DESIGN §6).
//
// Two kinds of instance live here and they are honestly different. An
// OpenAI-compatible endpoint is Owner-configured: label, base URL, API key,
// default model, all editable, removable. Codex and Claude are local OAuth
// credentials the host either can see or cannot; there is nothing to edit and
// no login this client could perform, so their whole surface is a detection
// reading and a re-probe.
//
// Secrets discipline: no full API key is ever rendered, logged, put in the DOM
// as a value, or held in the store. The only key material client-side is the
// transient contents of the password input during one edit, cleared on save or
// cancel.
import { LoaderCircle } from "lucide-solid";
import { createEffect, createSignal, For, type JSX, onMount, Show } from "solid-js";
import { createFocusTrap } from "../../lib/focus";
import { createPendingKeys } from "../../lib/pending";
import type { ProviderInstance } from "../../protocol";
import { state } from "../../store/store";
import { getClient } from "../../ws/client";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { settleOnProtocolError } from "./agent-config";
import { Group } from "./rows";

/** The id shape the host accepts, and the two ids it reserves for the built-in
 * OAuth instances. Checked here so a malformed id is refused inline instead of
 * making a round trip to be rejected. */
const ID_SHAPE = /^[a-z0-9][a-z0-9_-]{0,31}$/;
const RESERVED_IDS = ["codex", "claude"];

/** The CLI whose local credentials an OAuth instance is probing for. */
function cliName(instance: ProviderInstance): string {
  return instance.kind === "claude" ? "claude" : "codex";
}

/** What the host reported about the stored key — presence and a short tail are
 * the whole vocabulary the wire has. */
function keyState(instance: ProviderInstance): string {
  if (!instance.api_key?.present) return "No key set";
  const tail = instance.api_key.tail;
  return tail ? `Key set · ends ${tail}` : "Key set";
}

const FIELD_CLASS =
  "mt-1 h-9 rounded-lg border border-border bg-surface px-2.5 text-sm text-foreground transition-colors hover:border-input focus-visible:ring-2 focus-visible:ring-ring";

function EditField(props: {
  label: string;
  value: string;
  onInput: (value: string) => void;
  type?: string;
  placeholder?: string;
  mono?: boolean;
  disabled?: boolean;
  helper?: string;
}) {
  return (
    <div class="mt-2.5">
      <div class="text-xs text-muted-foreground">{props.label}</div>
      <Input
        aria-label={props.label}
        type={props.type}
        autocomplete="off"
        spellcheck={false}
        disabled={props.disabled}
        placeholder={props.placeholder}
        value={props.value}
        onInput={(event) => props.onInput(event.currentTarget.value)}
        class={props.mono ? `${FIELD_CLASS} font-mono text-xs` : FIELD_CLASS}
      />
      <Show when={props.helper}>
        <div class="mt-1 text-xs text-muted-foreground">{props.helper}</div>
      </Show>
    </div>
  );
}

/** Confirmation for a removal, on the ConfirmForgetDialog pattern: focus
 * trapped in the card, Escape cancels, the destructive verb names the thing. */
function ConfirmRemoveDialog(props: {
  label: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  let dialogRef: HTMLDivElement | undefined;
  onMount(() => {
    createFocusTrap(() => dialogRef, { onEscape: () => props.onCancel() });
  });

  return (
    // eslint-disable-next-line jsx-a11y/click-events-have-key-events, jsx-a11y/no-static-element-interactions
    <div
      class="fixed inset-0 z-50 flex items-center justify-center bg-background/70 p-6"
      onClick={(event) => {
        if (event.target === event.currentTarget) props.onCancel();
      }}
    >
      <div
        ref={(node) => {
          dialogRef = node;
        }}
        tabindex={-1}
        role="alertdialog"
        aria-label="Remove provider"
        class="w-full max-w-[320px] rounded-xl border border-border bg-card p-4 shadow-lg outline-none"
      >
        <h3 class="m-0 text-sm font-semibold text-foreground">Remove {props.label}?</h3>
        <p class="mt-1.5 mb-4 text-sm leading-relaxed text-muted-foreground">
          The host drops this instance and its stored key. Any agent pointed at it falls back to
          the provider it booted on.
        </p>
        <div class="flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={props.onCancel}>
            Cancel
          </Button>
          <Button variant="destructive" size="sm" onClick={props.onConfirm}>
            Remove provider
          </Button>
        </div>
      </div>
    </div>
  );
}

/** One OpenAI-compatible instance, in its resting reading or its open editor. */
function OpenAiRow(props: {
  instance: ProviderInstance;
  editing: boolean;
  busy: boolean;
  onEdit: () => void;
  onCancel: () => void;
  onSave: (patch: {
    label?: string;
    base_url?: string;
    api_key?: string;
    default_model?: string;
  }) => void;
  onRemove: () => void;
}) {
  const [label, setLabel] = createSignal(props.instance.label);
  const [baseUrl, setBaseUrl] = createSignal(props.instance.base_url ?? "");
  const [defaultModel, setDefaultModel] = createSignal(props.instance.default_model ?? "");
  // Never prefilled: the client has no key to prefill it with.
  const [apiKey, setApiKey] = createSignal("");
  const [refusal, setRefusal] = createSignal<string | null>(null);

  // Re-seed the draft whenever the editor opens, so a cancelled edit leaves no
  // residue and the key field starts empty every time.
  createEffect(() => {
    if (!props.editing) return;
    setLabel(props.instance.label);
    setBaseUrl(props.instance.base_url ?? "");
    setDefaultModel(props.instance.default_model ?? "");
    setApiKey("");
    setRefusal(null);
  });

  function save() {
    const nextLabel = label().trim();
    const nextBaseUrl = baseUrl().trim();
    const nextModel = defaultModel().trim();
    if (!nextLabel || !nextBaseUrl || !nextModel) {
      setRefusal("Label, base URL and default model are required.");
      return;
    }
    setRefusal(null);
    // An untouched key field omits `api_key` entirely: unchanged, not cleared.
    const key = apiKey();
    setApiKey("");
    props.onSave({
      label: nextLabel,
      base_url: nextBaseUrl,
      default_model: nextModel,
      ...(key.length > 0 ? { api_key: key } : {}),
    });
  }

  function cancel() {
    setApiKey("");
    setRefusal(null);
    props.onCancel();
  }

  return (
    <div class="py-3">
      <Show
        when={props.editing}
        fallback={
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0 flex-1">
              <div class="truncate text-sm text-foreground">{props.instance.label}</div>
              <div class="mt-0.5 truncate font-mono text-meta text-muted-foreground">
                {props.instance.base_url ?? "No base URL"}
              </div>
              <div class="mt-0.5 text-xs text-muted-foreground">
                {keyState(props.instance)} · {props.instance.default_model || "No default model"}
              </div>
            </div>
            <div class="flex shrink-0 items-center gap-1">
              <Show when={props.busy}>
                <LoaderCircle
                  class="size-3.5 animate-spin text-muted-foreground"
                  aria-label="Saving"
                />
              </Show>
              <Button
                variant="ghost"
                size="sm"
                class="h-9"
                disabled={props.busy}
                aria-label={`Edit ${props.instance.label}`}
                onClick={props.onEdit}
              >
                Edit
              </Button>
              <Show when={props.instance.removable}>
                <Button
                  variant="ghost"
                  size="sm"
                  class="h-9 text-status-danger hover:text-status-danger"
                  disabled={props.busy}
                  aria-label={`Remove ${props.instance.label}`}
                  onClick={props.onRemove}
                >
                  Remove
                </Button>
              </Show>
            </div>
          </div>
        }
      >
        <div class="text-sm text-foreground">{props.instance.label}</div>
        <EditField label="Label" value={label()} onInput={setLabel} disabled={props.busy} />
        <EditField
          label="Base URL"
          value={baseUrl()}
          onInput={setBaseUrl}
          mono
          disabled={props.busy}
        />
        <EditField
          label="API key"
          type="password"
          value={apiKey()}
          onInput={setApiKey}
          disabled={props.busy}
          placeholder="Leave empty to keep the stored key"
          helper={keyState(props.instance)}
        />
        <EditField
          label="Default model"
          value={defaultModel()}
          onInput={setDefaultModel}
          mono
          disabled={props.busy}
        />
        <Show when={refusal()}>
          <p class="mt-2 text-xs leading-snug text-muted-foreground">{refusal()}</p>
        </Show>
        <div class="mt-3 flex items-center gap-2">
          <Button size="sm" class="h-9" disabled={props.busy} onClick={save}>
            Save
          </Button>
          <Button variant="ghost" size="sm" class="h-9" disabled={props.busy} onClick={cancel}>
            Cancel
          </Button>
          <Show when={props.instance.api_key?.present}>
            <Button
              variant="ghost"
              size="sm"
              class="h-9 text-status-danger hover:text-status-danger"
              disabled={props.busy}
              aria-label={`Clear ${props.instance.label} key`}
              onClick={() => {
                setApiKey("");
                props.onSave({ api_key: "" });
              }}
            >
              Clear key
            </Button>
          </Show>
        </div>
      </Show>
    </div>
  );
}

/** Codex / Claude: local OAuth credentials the host either sees or does not.
 * No embedded login — the Owner logs in on the host machine and re-probes. */
function DetectedRow(props: {
  instance: ProviderInstance;
  busy: boolean;
  onRedetect: () => void;
}) {
  const detection = () => props.instance.detection;
  return (
    <div class="py-3">
      <div class="flex items-start justify-between gap-3">
        <div class="min-w-0 flex-1">
          <div class="truncate text-sm text-foreground">{props.instance.label}</div>
          <div class="mt-0.5 text-xs text-muted-foreground">
            <Show when={detection()} fallback="Not reported">
              {(status) =>
                status().detected
                  ? status().account_hint
                    ? `Detected · ${status().account_hint}`
                    : "Detected"
                  : "Not detected"
              }
            </Show>
          </div>
          <Show when={detection()?.path}>
            <div class="mt-0.5 truncate font-mono text-meta text-muted-foreground">
              {detection()?.path}
            </div>
          </Show>
          <Show when={detection() && !detection()?.detected}>
            <Show when={detection()?.detail}>
              <p class="mt-1 text-xs leading-snug text-muted-foreground">{detection()?.detail}</p>
            </Show>
            <p class="mt-1 text-xs leading-snug text-muted-foreground">
              Log in with the {cliName(props.instance)} CLI on the host machine, then check again.
            </p>
          </Show>
          <Show when={props.instance.kind === "claude"}>
            <p class="mt-1 text-xs leading-snug text-muted-foreground">
              Available to Sub-agents only — it cannot run the main Agent or the fork.
            </p>
          </Show>
        </div>
        <div class="flex shrink-0 items-center gap-1">
          <Show when={props.busy}>
            <LoaderCircle
              class="size-3.5 animate-spin text-muted-foreground"
              aria-label="Checking"
            />
          </Show>
          <Button
            variant="ghost"
            size="sm"
            class="h-9"
            disabled={props.busy}
            aria-label={`Check ${props.instance.label} again`}
            onClick={props.onRedetect}
          >
            Check again
          </Button>
        </div>
      </div>
    </div>
  );
}

/** The new-instance form: everything an OpenAI-compatible endpoint needs, with
 * the id validated here so a reserved or malformed one never leaves the tab. */
function AddProviderForm(props: {
  busy: boolean;
  onCancel: () => void;
  onAdd: (instance: {
    id: string;
    label: string;
    base_url: string;
    api_key: string;
    default_model: string;
  }) => void;
}) {
  const [id, setId] = createSignal("");
  const [label, setLabel] = createSignal("");
  const [baseUrl, setBaseUrl] = createSignal("");
  const [apiKey, setApiKey] = createSignal("");
  const [defaultModel, setDefaultModel] = createSignal("");
  const [refusal, setRefusal] = createSignal<string | null>(null);

  function add() {
    const nextId = id().trim();
    if (RESERVED_IDS.includes(nextId)) {
      setRefusal("codex and claude are built in — choose another id.");
      return;
    }
    if (!ID_SHAPE.test(nextId)) {
      setRefusal("An id is lower-case letters, digits, - or _, up to 32 characters.");
      return;
    }
    const nextLabel = label().trim();
    const nextBaseUrl = baseUrl().trim();
    const nextModel = defaultModel().trim();
    if (!nextLabel || !nextBaseUrl || !nextModel) {
      setRefusal("Label, base URL and default model are required.");
      return;
    }
    setRefusal(null);
    const key = apiKey();
    setApiKey("");
    props.onAdd({
      id: nextId,
      label: nextLabel,
      base_url: nextBaseUrl,
      api_key: key,
      default_model: nextModel,
    });
  }

  return (
    <div class="py-3">
      <div class="text-sm text-foreground">New provider</div>
      <EditField
        label="Provider id"
        value={id()}
        onInput={setId}
        mono
        disabled={props.busy}
        placeholder="openrouter-2"
      />
      <EditField label="Provider label" value={label()} onInput={setLabel} disabled={props.busy} />
      <EditField
        label="Provider base URL"
        value={baseUrl()}
        onInput={setBaseUrl}
        mono
        disabled={props.busy}
        placeholder="https://openrouter.ai/api/v1"
      />
      <EditField
        label="Provider API key"
        type="password"
        value={apiKey()}
        onInput={setApiKey}
        disabled={props.busy}
        helper="Stored on the host. It is never sent back to this browser."
      />
      <EditField
        label="Provider default model"
        value={defaultModel()}
        onInput={setDefaultModel}
        mono
        disabled={props.busy}
      />
      <Show when={refusal()}>
        <p class="mt-2 text-xs leading-snug text-muted-foreground">{refusal()}</p>
      </Show>
      <div class="mt-3 flex items-center gap-2">
        <Button size="sm" class="h-9" disabled={props.busy} onClick={add}>
          Add provider
        </Button>
        <Button
          variant="ghost"
          size="sm"
          class="h-9"
          disabled={props.busy}
          onClick={() => {
            setApiKey("");
            props.onCancel();
          }}
        >
          Cancel
        </Button>
      </div>
    </div>
  );
}

export function ProvidersSection(): JSX.Element {
  const roster = () => state.providers;
  const [editing, setEditing] = createSignal<string | null>(null);
  const [adding, setAdding] = createSignal(false);
  const [confirmRemove, setConfirmRemove] = createSignal<ProviderInstance | null>(null);

  // The host broadcasts the whole roster after an accepted edit; that frame is
  // the only settle signal a write here has. Equal rosters are still
  // acknowledgements, so the revision counter is what this tracks.
  const pending = createPendingKeys();
  let awaitingFrame = false;
  createEffect(() => {
    void state.providersRevision;
    if (awaitingFrame) {
      awaitingFrame = false;
      setEditing(null);
      setAdding(false);
    }
    pending.settleAll();
  });
  settleOnProtocolError(pending);

  function write(key: string, send: () => void) {
    awaitingFrame = true;
    pending.begin(key);
    send();
  }

  return (
    <Show
      when={roster()}
      fallback={
        <p class="text-xs leading-snug text-muted-foreground">
          This host reports no provider roster.
        </p>
      }
    >
      {/* Degraded but running: the host fell back to its environment default
        * because a stored choice could not boot. DESIGN §2 keeps amber for
        * waiting and coral for genuine blockage, so this standing line speaks
        * in the muted voice — it reports, it does not alarm. */}
      <Show when={roster()?.boot_notice}>
        <p class="mb-2 text-xs leading-snug text-muted-foreground">{roster()?.boot_notice}</p>
      </Show>
      <Group class="divide-y divide-border">
        <For each={roster()?.instances ?? []}>
          {(instance) => (
            <Show
              when={instance.kind === "openai_compatible"}
              fallback={
                <DetectedRow
                  instance={instance}
                  busy={pending.isPending(instance.id)}
                  onRedetect={() =>
                    write(instance.id, () => getClient()?.redetectProvider(instance.id))
                  }
                />
              }
            >
              <OpenAiRow
                instance={instance}
                editing={editing() === instance.id}
                busy={pending.isPending(instance.id)}
                onEdit={() => {
                  setAdding(false);
                  setEditing(instance.id);
                }}
                onCancel={() => setEditing(null)}
                onSave={(patch) =>
                  write(instance.id, () => getClient()?.updateProvider(instance.id, patch))
                }
                onRemove={() => setConfirmRemove(instance)}
              />
            </Show>
          )}
        </For>
        <Show
          when={adding()}
          fallback={
            <div class="py-3">
              <Button
                variant="ghost"
                size="sm"
                class="h-9"
                onClick={() => {
                  setEditing(null);
                  setAdding(true);
                }}
              >
                Add provider
              </Button>
            </div>
          }
        >
          <AddProviderForm
            busy={pending.isPending("add")}
            onCancel={() => setAdding(false)}
            onAdd={(instance) => write("add", () => getClient()?.addProvider(instance))}
          />
        </Show>
      </Group>
      <p class="mt-2 text-xs leading-snug text-muted-foreground">
        Keys are stored on the host. This browser only ever learns that a key is set and how it
        ends.
      </p>
      <Show when={confirmRemove()}>
        {(instance) => (
          <ConfirmRemoveDialog
            label={instance().label}
            onCancel={() => setConfirmRemove(null)}
            onConfirm={() => {
              const id = instance().id;
              setConfirmRemove(null);
              write(id, () => getClient()?.removeProvider(id));
            }}
          />
        )}
      </Show>
    </Show>
  );
}
