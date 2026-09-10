# C19-WEB-SETTINGS audit

## Result

Two high-confidence, materially useful recommendations survive the
representation and ownership review:

1. Remove the dead, persisted “Debug mode” setting.
2. Remove the provider editor’s duplicate awaitingFrame latch and use the
   existing pending-write state as the only acknowledgement owner.

No source file was edited. No test, build, install, migration, commit, push,
live-data, provider, session, or process action was performed. The report is
the only file written, at the path assigned by the dispatch.

## Snapshot and scope

- Repository: /workspace/code/hirsel-audit-c19
- Expected and observed HEAD: 3ee0621a603659ab0168f565b99012b642415419
- Expected and observed HEAD tree: a4aac830c45398a66591f2c44b707aaf3cef281b
- Pre-audit source worktree: clean (git status --porcelain and
  git diff --name-only produced no output)
- Scope: the 21 whole-file owners named by the specification, plus the
  explicitly permitted protocol, store, websocket, pending, app, and plugin
  consumer context
- Shared exact definitions assigned to C19: none
- Finding cap: two; no third lead is left open

The audit used both required lenses: representable-but-invalid or duplicate
state, and unnecessary state/control-flow/collection complexity. Findings are
limited to C19-owned UI state and behavior. Provider/model protocol contracts
and plugin runtime/persistence types were inspected for consumer context, but
their ownership remains outside this cluster.

## Finding C19-01 — dead “Debug mode” setting

Verdict: recommend removal. Confidence: high.

### Evidence and reachability

The About surface promises behavior that does not exist:

> app/src/components/settings/AboutSection.tsx:28-30 renders
> Field title="Debug mode" subtitle="Verbose client logging for diagnostics."
> and a Toggle whose value is supplied by the parent.

The parent creates and persists a local value:

> app/src/components/settings/SettingsSheet.tsx:45-46 initializes debug from
> readLocal(DEBUG_KEY) === "1".

> app/src/components/settings/SettingsSheet.tsx:89-96 updates the signal and
> writes DEBUG_KEY to localStorage when the toggle changes.

> app/src/components/settings/SettingsSheet.tsx:98-112 includes the value in
> the copied diagnostics blob; line 107 reports the selected on/off value.

The storage key is defined only as a settings preference:

> app/src/components/settings/prefs.ts:4-8 says the local preferences are
> “a cosmetic display label and a debug flag” and are never sent to the Host.

> app/src/components/settings/prefs.ts:12-13 defines DEVICE_LABEL_KEY and
> DEBUG_KEY = "hirsel.debug".

The user can reach this state by opening Settings → About, toggling Debug
mode, reopening the sheet, and copying diagnostics. The localStorage value and
the displayed diagnostics survive that path. Nothing in the client logging
behavior changes.

### Consumer query and invalid state

Reproducible query:

    rg -n "DEBUG_KEY|hirsel\.debug|debugEnabled" app/src --glob '!app/src/components/settings/**'

Result: 0 matches (the command exits with status 1 for no matches).

The corresponding owned-surface matches are the key definition, import,
initialization, and write cited above. A second query found the only five
logging call sites in the client:

    rg -n "console\.(debug|log|info|warn|error)" app/src

Result: 5 matches:

    app/src/ws/client.ts:476
    app/src/plugins/loader.ts:115
    app/src/plugins/loader.ts:127
    app/src/plugins/loader.ts:144
    app/src/plugins/registry.ts:101

None reads the setting or is gated by it. The concrete ineffectual state is
therefore: localStorage contains hirsel.debug=1, the Settings signal is true,
and the client still has exactly the same logging behavior as when the value
is 0. This is a reachable orphaned state, not merely a hypothetical future
mismatch.

There is no second runtime authority or competing write path: the signal is a
UI projection of localStorage, and no logger consumes either value. The
duplicate truth is instead the unnecessary persisted/UI representation of a
nonexistent capability. The diagnostics string is an additional observable
surface that makes the dead state look operative.

### Proposed representation and cutover

Delete the state at every C19 layer:

- AboutSection.tsx: remove debug and onDebugChange from the component props
  and remove the Debug mode row.
- SettingsSheet.tsx: remove the DEBUG_KEY import, the debug signal,
  toggleDebug, the About props, and the diagnostics debug line.
- prefs.ts: remove DEBUG_KEY; retain device-label storage and the pure
  helpers used by Settings.
- Protocol, store, Host, persistence, and plugin layers: add no replacement
  field. There is no real debug capability to preserve.

This is a deletion, not a new boolean or table column. Existing
hirsel.debug browser values can simply become ignored; no migration is
needed because no runtime behavior depends on them. The smallest affected
interfaces are the AboutSection props and the SettingsPanel local
preference/diagnostics code.

### Risk and validation

The risk is limited to users or out-of-repository tooling that may expect the
visible row, copied debug diagnostics field, or localStorage key. No
in-repository consumer was found. If verbose logging is an actual product
requirement, the alternative is a larger, separately specified logging
capability with one shared runtime gate; the current setting cannot be
described as that capability.

Existing validation does not cover the defect: settings.test.tsx covers
device-label persistence, host version, Show agent code persistence, Settings
chrome, focus, and forget-token confirmation, but has no Debug mode or logging
assertion. Inspection-only follow-up should confirm that DEBUG_KEY,
hirsel.debug, and the Debug mode label have no remaining in-repository
consumers and that diagnostics no longer advertises the removed state. No
tests were run in this audit.

## Finding C19-02 — provider acknowledgement latch diverges after errors

Verdict: recommend removing the duplicate latch. Confidence: high.

### Evidence and ownership path

The provider section maintains two representations of the same write
lifecycle:

> app/src/components/settings/ProvidersSection.tsx:474-484 creates pending,
> separately sets awaitingFrame = false, closes editing and adding when the
> roster revision changes, then calls pending.settleAll().

> app/src/components/settings/ProvidersSection.tsx:486-489 sets
> awaitingFrame = true, begins a pending key, and sends the write.

The shared pending interface already exposes the needed aggregate state:

> app/src/lib/pending.ts:19-30 defines isPending, any, begin, settle, and
> settleAll; any is explicitly “Is ANY key pending?”

> app/src/lib/pending.ts:63-66 makes settleAll() clear all timers and replace
> the pending key set with an empty set.

The C19 error helper settles only that existing pending representation:

> app/src/components/settings/agent-config.tsx:33-40 observes
> state.protocolError and calls pending.settleAll() for a new non-null error.

The websocket/store path makes the two event classes distinct:

> app/src/ws/client.ts:418-420 dispatches an accepted providers_changed roster
> frame.

> app/src/ws/client.ts:468-474 turns an uncorrelated post-auth protocol error
> into setProtocolError(message.detail) and emits no provider roster frame.

> app/src/store/store.ts:15-18 replaces the provider snapshot and increments
> providersRevision only for providers_changed.

The editor and add form hold local drafts while their parent flags are active:

> app/src/components/settings/ProvidersSection.tsx:142-151 stores the
> OpenAI-compatible row’s label, base URL, default model, and empty API-key
> draft, and reseeds the draft when the editor opens.

> app/src/components/settings/ProvidersSection.tsx:523-535 mounts the row
> while editing() selects it and sends edits through write.

> app/src/components/settings/ProvidersSection.tsx:540-562 mounts AddProviderForm
> while adding() is true and sends additions through the same write function.

### Reachable invalid combination

The exact sequence is:

1. Open a provider editor or the Add provider form and submit a write.
   write sets awaitingFrame true and adds a pending key.
2. The Host rejects the command with an uncorrelated, post-auth error.
   settleOnProtocolError clears the pending set, but no code resets
   awaitingFrame.
3. The user continues editing the still-mounted form, or simply leaves it
   open.
4. A later accepted provider update from another action, another client, or
   provider redetection dispatches providers_changed.
5. The providersRevision effect sees stale awaitingFrame, calls
   setEditing(null) or setAdding(false), and destroys the open draft.

The concrete invalid state is awaitingFrame === true with pending.any() ===
false after the failed write, while editing() or adding() still identifies an
active draft. A later unrelated roster update is then incorrectly treated as
the acknowledgement for the failed command. This is reachable with the
current error and broadcast paths; it does not require a malformed packet or
a hypothetical API.

Reproducible ownership query:

    rg -n "awaitingFrame|settleOnProtocolError|providersRevision" app/src/components/settings/ProvidersSection.tsx app/src/components/settings/agent-config.tsx app/src/store/store.ts

Result: 12 matches, specifically the store revision field/initialization/
increment, the shared error-settlement definition and use, and the
provider-section latch, revision effect, settle call, and write assignment:

    app/src/store/store.ts:6
    app/src/store/store.ts:7
    app/src/store/store.ts:17
    app/src/components/settings/agent-config.tsx:33
    app/src/components/settings/agent-config.tsx:612
    app/src/components/settings/ProvidersSection.tsx:26
    app/src/components/settings/ProvidersSection.tsx:475
    app/src/components/settings/ProvidersSection.tsx:476
    app/src/components/settings/ProvidersSection.tsx:477
    app/src/components/settings/ProvidersSection.tsx:478
    app/src/components/settings/ProvidersSection.tsx:484
    app/src/components/settings/ProvidersSection.tsx:487

The provider test query has one successful-broadcast case and no protocol
error case:

    rg -n "providers_changed|protocolError|setProtocolError|providersRevision" app/src/components/settings/settings-providers.test.tsx app/src/components/settings/settings-agents.test.tsx app/src/store/reducer.model.test.ts app/src/ws/client.test.ts

The relevant matches are settings-providers.test.tsx:207-213 for an open
editor closing after providers_changed, and agent pending-error cases in
settings-agents.test.tsx:268 and settings-agents.test.tsx:405. There is no
test combining a provider write rejection with a later unrelated provider
roster update.

### Proposed representation and cutover

Delete awaitingFrame and make pending the sole write-liveness
representation. The revision effect should close the editor/add form only
when the roster frame arrives while a provider write remains pending, then
settle that pending set:

    const pending = createPendingKeys();
    createEffect(() => state.providersRevision, () => {
      if (pending.any()) {
        setEditing(null);
        setAdding(false);
      }
      pending.settleAll();
    });
    settleOnProtocolError(pending);

write should only call pending.begin(key) before sending. No protocol, store,
Host, persistence, or shared interface change is required. The smallest
implementation scope is ProvidersSection.tsx; the regression coverage
belongs in settings-providers.test.tsx.

This removes the invalid combination because the only value consulted by the
acknowledgement effect is also the value cleared by protocol errors. An error
leaves no pending write, so an unrelated later roster frame cannot close a
draft. An accepted frame still closes the editor/add form while the write is
pending, preserving the current successful-acknowledgement behavior.

### Risk and validation

The intentional behavior change is that a roster refresh with no pending
local provider write no longer closes an open draft. That is the desired
ownership rule: read-only external refreshes must not destroy local input.
The existing successful providers_changed test should remain valid.

Inspection-only follow-up should add a provider test that starts an edit,
submits a write, settles it through protocolError, dispatches an unrelated
providers_changed, and asserts that the editor and its draft remain. Keep
the existing successful-broadcast close assertion. A second case should
cover the Add provider form. No tests were run in this audit.

## Coverage contract

Every assigned file was opened and read as a whole. The exact definitions or
surfaces inspected were:

| Owned file | Definitions / exact surface inspected | Disposition |
| --- | --- | --- |
| AboutSection.tsx | AboutSection; version, host-version, Debug mode, Show agent code, diagnostics controls | C19-01 |
| AgentsSection.tsx | EMPTY_PROMPT, MainAgent, SubagentModelRow, SubagentModels, AgentsSection; model/provider/prompt/subagent catalog state and writes | No additional finding |
| AppearanceSection.tsx | AppearanceSection; theme segmented control and ThemeMode consumer | No finding |
| ConnectionSection.tsx | ConnectionSection, ConfirmForgetDialog; endpoint, token masking, forget flow | No finding |
| ForkAgentSection.tsx | ForkAgentSection; fork provider/model/prompt controls and local drafts | No additional finding |
| GuideSection.tsx | P, Key, Shortcut, GuideSection; static guide/shortcut rendering | No finding |
| IdentitySection.tsx | IdentitySection; local device-label draft, trim/changed checks, fingerprint display | No finding |
| NotificationsSection.tsx | NotificationsSection; browser support/permission state and desktop-notification copy | No finding promoted |
| PluginsSection.tsx | StateBadge, SettingsForm, PluginRow, PluginsSection; plugin list, setting descriptors/values, secret omission, enable/save refresh | No C19-owned finding |
| ProvidersSection.tsx | ID_SHAPE, RESERVED_IDS, cliName, keyState, EditField, ConfirmRemoveDialog, OpenAiRow, DetectedRow, AddProviderForm, ProvidersSection | C19-02 |
| SettingsSheet.tsx | SETTINGS_COLUMN, SettingsPanel, SettingsSheet; tab selection, local prefs, diagnostics, mounted sections, focus/forget flows | C19-01 context; no other finding |
| SettingsTabs.tsx | SETTINGS_TABS, settingsTabId, settingsPanelId, SettingsTabs; tab registry, ARIA, roving keyboard navigation | No finding |
| agent-config.tsx | settleOnProtocolError, agentProviders, providerLabel, AgentProviderRow, AgentModelView, agentModelView, FreeTextModelRow, AgentModelRows, PromptActions, ExpandedPromptEditor, PromptEditor, createAgentPending | Shared error-settlement context; no additional finding |
| prefs.ts | DEVICE_LABEL_KEY, DEBUG_KEY, PHASE_WORD, readLocal, computeFingerprint, maskToken, copyText, titleCase | C19-01 |
| rows.tsx | Group, SectionHeader, Field, SegmentedControl, Toggle, Select, SubHeading, CopyRow | Generic primitives; no finding |
| settings-agents.test.tsx | Main-agent, sub-agent, provider filtering/switching, curated/free-text model, prompt and error fixtures/tests | Existing coverage; no additional finding |
| settings-plugins.test.tsx | Plugin listing/empty, enable, settings string/boolean/secret, masked-secret omission/retype fixtures/tests | Existing coverage; no C19-owned finding |
| settings-prompts.test.tsx | Prompt snapshots, save/reset, fork, expanded editor, focus, broadcast settlement | Existing coverage; no additional finding |
| settings-providers.test.tsx | Roster, masked key, edit/save/clear, validation, remove, detection, successful roster settlement | Existing coverage exposes missing error regression |
| settings-tabs.test.tsx | Tab labels, active-only mounting, ARIA, keyboard/roving tabindex, landing tab, Guide, command entry point | Existing coverage; no finding |
| settings.test.tsx | Device label, host version, Show agent code, sheet chrome/focus, forget-token confirmation | Existing coverage exposes missing Debug assertion |

The permitted consumer context was also inspected: protocol model/provider/
prompt/catalog/provider-roster types; store state/actions/reducer/selectors;
websocket send/receive/error handling; pending-key implementation; App
notification and Show agent code consumers; and plugin host/loader/registry/
slot, HTTP conversion, runtime state, and storage paths. No C19-owned defect
was found in those shared or adjacent representations.

## Explicit skips and near misses

- The Settings tab union, tab array, ID helpers, and render switch are three
  local mappings, but they are a normal closed navigation representation.
  TypeScript constrains the IDs and settings-tabs.test.tsx covers the
  mapping. No reachable invalid state or independent write authority justified
  a finding.
- Provider/model snapshots and their optional/tagged selections were traced
  through Host and protocol conversion. The host normalizes these snapshots;
  their ownership is outside C19 and no uncoupled duplicate write path was
  found.
- Plugin state/error and settings descriptor/value shapes are owned by the
  plugin runtime/API/storage surface. C19 renders and submits them. The
  plugin consumer and refresh paths were inspected, but no C19-owned
  representation defect met the materiality bar.
- SettingsSheet.tsx:106 hardcodes “notifications: not available (web)” while
  NotificationsSection.tsx:14-22,44-65 and App.tsx:97-126 implement browser
  notification permission and delivery. This is a confirmed stale diagnostics
  string, but it is a low-scope copy defect rather than a materially useful
  representation/control-flow simplification; it was not promoted under the
  two-finding cap.
- No stylistic, hypothetical, minor, intentional denormalization, or
  adjacent-cluster finding was retained. C14 protocol/connection and C23
  plugin ownership were treated as deduplication boundaries.

## Final verification

After report creation, the source snapshot was rechecked:

    git rev-parse HEAD
    git rev-parse HEAD^{tree}
    git status --porcelain
    git diff --name-only

It remained at HEAD
3ee0621a603659ab0168f565b99012b642415419, tree
a4aac830c45398a66591f2c44b707aaf3cef281b, with empty status and diff-name
output. The report path is outside the repository, so it does not alter the
source worktree.

## Handoff

The audit is complete with exactly two verified recommendations. C19-01 is a
dead persisted/UI preference with zero runtime consumers. C19-02 is a
reachable provider-draft loss caused by a latch that protocol-error
settlement does not clear. Both have exact evidence, consumer queries,
smallest proposed cutovers, risks, existing tests, and required regression
validation. Root implementation and publication remain outside this
read-only worker’s authority.

Fix C19-01 first: remove the dead Debug mode state and its misleading diagnostics field; then remove the provider acknowledgement latch and add the two provider error-preservation regressions.
