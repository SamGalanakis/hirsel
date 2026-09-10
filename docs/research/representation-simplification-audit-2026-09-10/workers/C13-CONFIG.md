# C13-CONFIG — configuration, provider, model, prompt, and skill representations

## Scope, snapshot, and method

This is a read-only combined `schemasmash` and `audit-your-codebase` review of
the exact C13 whole-file ownership plus the two shared definitions named in the
dispatch. I inspected the owned Rust source, the protocol types, the specified
web settings consumers, the relevant tests/fixtures, the supplied exclusions,
and the sibling C02 report for deduplication. I did not read live configuration
or provider/auth data, execute application code, run tests/builds, or modify
repository source.

Initial repository verification:

```text
HEAD  3ee0621a603659ab0168f565b99012b642415419
tree  a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain  (empty)
```

The report has at most two findings. C13-01 is the only high-confidence
correctness issue. C13-02 is a medium-confidence latent wire-state issue whose
value is highest if this protocol surface is being changed; current host
constructors do not emit the bad states.

## Findings

### C13-01 — persisted custom-provider rows bypass the writer invariant

**Verdict: recommend; high confidence.** The read boundary for
`[providers.<id>]` accepts an empty `base_url` and an absent or empty
`default_model`, although every normal add/update path requires both to be
non-empty. This makes a hand-edited row look valid to the roster and, with a
key, can reach provider boot with an unusable endpoint. The same row can also
be offered as a selectable provider that has no model to seed.

**Owned representation and parser.** `StoredProvider` declares these fields
as ordinary `String`s:

```text
crates/hirsel-host/src/host_config/provider_store.rs:21-31
pub struct StoredProvider {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub default_model: String,
}
```

The parser checks only that `base_url` exists as a TOML string, then copies it;
it defaults a missing model to the empty string:

```text
crates/hirsel-host/src/host_config/provider_store.rs:74-87
let kind = ...unwrap_or_default();
if kind != OPENAI_COMPATIBLE_KIND { ... continue; }
let Some(base_url) = section.get("base_url").and_then(Item::as_str) else {
    ... continue;
};

crates/hirsel-host/src/host_config/provider_store.rs:89-106
base_url: base_url.to_string(),
...
default_model: section
    .get("default_model")
    .and_then(Item::as_str)
    .unwrap_or_default()
    .to_string(),
```

That contradicts the parser's own contract that a malformed entry is skipped
(`provider_store.rs:55-58`). In contrast, the ordinary writer path rejects
empty or whitespace-only values:

```text
crates/hirsel-host/src/providers.rs:216-223
label: non_empty(label, "label")?.to_string(),
base_url: non_empty(base_url, "base_url")?.to_string(),
...
default_model: non_empty(default_model, "default_model")?.to_string(),

crates/hirsel-host/src/providers.rs:239-251
provider.base_url = non_empty(base_url, "base_url")?.to_string();
provider.default_model = non_empty(default_model, "default_model")?.to_string();

crates/hirsel-host/src/providers.rs:408-414
fn non_empty(...) -> anyhow::Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() { return Err(...); }
    Ok(trimmed)
}
```

**Concrete state and reachability.** This hand-authored file is accepted by
`ConfigStore::providers()` (the `default_model` line may be omitted):

```toml
[providers.bad]
kind = "openai_compatible"
base_url = ""
api_key = "sk-example-key"

[model]
provider = "bad"
id = "whatever"
variant = "default"
```

This is **reachable through the persisted TOML input** and is latent through
the normal Settings/add/update commands, which already validate the fields.
No current production call was found that intentionally writes the invalid
row; the gap is specifically the manual/config-file read path. I did not read
any live `hirsel.toml`.

The downstream path does not reject the empty endpoint:

```text
crates/hirsel-host/src/boot_provider.rs:166-182
let Some(stored) = store.providers()...find(...) else { ... };
let Some(api_key) = stored.api_key.filter(...) else { ... };
plan: BootPlan::OpenAiCompatible {
    id: stored.id,
    base_url: stored.base_url,
    api_key,
},

crates/hirsel-host/src/lash_runtime/provider.rs:102-107
BootPlan::OpenAiCompatible { base_url, api_key, .. }
    => Ok(openai_compatible_handle(api_key.clone(), base_url.clone())),

crates/hirsel-host/src/lash_runtime/provider.rs:129-145
let mut provider = OpenAiCompatibleProvider::new(api_key, base_url);
```

The missing model has a separate visible failure path. It is copied into the
provider choice (`providers.rs:163-167`), becomes a free-text selection mode
(`model_selection.rs:159-163`), and cannot produce a default selection because
`default_in_mode` validates the empty string (`model_selection.rs:220-225`).
The public provider-selection command reports the resulting seed failure:

```text
crates/hirsel-host/src/lib.rs:366-388
default_model: choice.default_model.clone(),
...
default_in_mode(...).ok_or_else(||
    anyhow!("provider `{}` offers no default model to seed; set one first", ...)
)
```

**Consumer query.** Reproducible query and result count:

```sh
rg -n 'providers\(\)|stored\(|BootPlan::OpenAiCompatible|OpenAiCompatibleProvider::new|stored\.default_model|stored\.base_url' \
  crates/hirsel-host/src/host_config/provider_store.rs \
  crates/hirsel-host/src/providers.rs \
  crates/hirsel-host/src/boot_provider.rs \
  crates/hirsel-host/src/lash_runtime/provider.rs \
  crates/hirsel-host/src/model_selection.rs
# 32 matching lines, including tests and definitions
```

**Smallest target.** Keep the TOML shape and `StoredProvider` fields, but make
`ConfigStore::providers()` parse each entry through one local validation helper
(for example `parse_stored_provider(id, section) -> Result<StoredProvider,
String>`). Require `base_url.trim()` and `default_model.trim()` to be non-empty,
store the trimmed values, and skip the row with an id/field-only warning. Keep
the current `label` fallback to the provider id and empty-key-to-`None`
normalization; neither is the defect. This is a two-file change in the likely
minimum implementation (`host_config/provider_store.rs` plus a shared/private
validator location in `providers.rs` or the store), with no protocol or schema
change. The ideal boundary is the store parser, so boot, roster, and model
selection all receive the same validated representation.

This removes the invalid state before it can become either
`BootPlan::OpenAiCompatible { base_url: "" }` or a free-text provider with no
seed model. It does not introduce URL syntax validation beyond the existing
writer contract, avoiding an unrelated policy change.

**Duplicate truth check.** No duplicate-truth write path was found for this
finding. `StoredProvider` is a projection of the TOML row; `ConfigStore`'s
`document`/`source` pair is an intentional hot-reload/cache and change-detection
mechanism. The actual issue is inconsistent validation at the read and write
boundaries, not two stores that can diverge.

**Existing and additional validation.** Existing source tests demonstrate a
valid round trip and that a hand-written row *missing* `base_url` is ignored
(`crates/hirsel-host/src/host_config/provider_store.rs:385-418`), and provider
command tests exercise valid add/update values and key clearing
(`crates/hirsel-host/src/providers.rs:470-564`). They do not demonstrate an
empty/whitespace `base_url`, a missing/empty `default_model`, or boot fallback
for such a row. Add inspection-backed tests for those raw TOML cases, one valid
round trip, and a boot-resolution case proving an invalid row is omitted; also
assert warnings contain no key material. No tests were executed in this audit.

**Risk and confidence.** Low implementation risk: it narrows the existing
"malformed entries are ignored" contract and leaves the file format unchanged.
The main regression risk is accidentally rejecting a legitimate provider whose
field is only whitespace; that is already rejected by the command writer.
Confidence: **high**.

### C13-02 — provider roster wire state is an unchecked cross-product

**Verdict: recommend as a bounded direct wire cutover; medium confidence.**
`ProviderInstance` describes three semantically different provider variants as
one flat struct with optional fields and independent booleans. The host's three
constructors always emit coherent combinations, but the Rust wire type and the
TypeScript mirror admit combinations that contradict the provider kind and the
host's own command rules.

**Definitions and producer evidence.** The protocol first defines the kind,
secret, detection, and selection vocabularies:

```text
crates/hirsel-proto/src/providers.rs:7-24
ProviderKind = Codex | Claude | OpenAiCompatible

crates/hirsel-proto/src/providers.rs:26-50
MaskedSecret { present: bool, tail: String }
DetectionStatus {
    detected: bool,
    path: String,
    account_hint: Option<String>,
    detail: Option<String>,
}

crates/hirsel-proto/src/providers.rs:52-66
ProviderSelection = Curated { main, fork } | FreeText
```

The instance then combines them without a kind-specific type:

```text
crates/hirsel-proto/src/providers.rs:68-90
pub struct ProviderInstance {
    pub id: String,
    pub kind: ProviderKind,
    pub label: String,
    pub base_url: Option<String>,
    pub api_key: MaskedSecret,
    pub default_model: String,
    pub detection: Option<DetectionStatus>,
    pub agent_selectable: bool,
    pub selection: Option<ProviderSelection>,
    pub removable: bool,
}
```

The TypeScript consumer widens the same fields further:

```text
app/src/protocol.ts:172-212
export interface ProviderInstance {
  id: string;
  kind: ProviderKind;
  label: string;
  base_url?: string;
  api_key?: MaskedSecret;
  default_model?: string;
  detection?: DetectionStatus;
  agent_selectable: boolean;
  selection?: ProviderSelection;
  removable: boolean;
}
```

The only host producers inspected are coherent by convention:

```text
crates/hirsel-host/src/providers.rs:281-309
codex: base_url None, detection Some, agent_selectable true,
       curated selection, removable false
claude: base_url None, detection Some, agent_selectable false,
        selection None, removable false

crates/hirsel-host/src/providers.rs:366-378
custom: kind OpenAiCompatible, base_url Some(...), detection None,
        agent_selectable true, selection FreeText, removable true
```

**Concrete invalid state and reachability.** This object is representable by
Rust `serde` and by the TypeScript interface, despite contradicting the
provider rules:

```json
{
  "id": "claude",
  "kind": "claude",
  "label": "Claude",
  "base_url": "https://example.invalid/v1",
  "api_key": {"present": true, "tail": "abcd"},
  "default_model": "m",
  "detection": null,
  "agent_selectable": true,
  "selection": {"mode": "free_text"},
  "removable": true
}
```

It is **latent**, not an observed current-host write: the three constructors
above do not produce it, and the host rejects Claude as an agent choice
(`crates/hirsel-host/src/providers.rs:173-179`). But if such a frame is
deserialized or assembled, the client filters only the independent boolean
(`app/src/components/settings/agent-config.tsx:42-47`), while provider settings
choose the renderer by `kind` and separately use the optional fields and
`removable` flag (`app/src/components/settings/ProvidersSection.tsx:291-331`;
`509-537`). The result can be a Claude row in an agent-provider picker followed
by a host rejection, or a built-in row with a remove action. The same problem
exists in smaller form for `DetectionStatus` (`detected: false` plus
`account_hint`) and `MaskedSecret` (`present: false` plus a non-empty `tail`).

**Consumer query.** Reproducible direct consumer query and result count:

```sh
rg -n 'instance\.(kind|base_url|api_key|default_model|detection|agent_selectable|selection|removable)|status\.(detected|path|account_hint|detail)' \
  app/src/components/settings app/src/protocol.ts
# 14 matching lines
```

**Smallest target.** Replace the flat Rust struct and TS interface with a
kind-discriminated union, updated atomically across the host protocol and the
settings client. A minimal shape is:

```text
ProviderInstance =
  Codex { id, label, detection, default_model, curated_selection }
  | Claude { id, label, detection }
  | OpenAiCompatible { id, label, base_url, api_key, default_model }
```

Make `DetectionStatus` a tagged `Detected { path, account_hint } | Undetected
{ path, detail }` enum/union and `MaskedSecret` an `Absent | Present { tail }`
enum/union. Preserve the deliberate `Curated { main, fork }` distinction. Derive
`agent_selectable` (Claude is false) and `removable` (only custom is true) from
the variant instead of serializing two more independent booleans. In the
TypeScript settings code, filter by the variant and render the corresponding
variant; do not retain cross-kind optional fields. The roster container and its
`booted_provider_id`/`boot_notice` fields remain unchanged. A direct cutover
touches at least `crates/hirsel-proto/src/providers.rs`,
`crates/hirsel-host/src/providers.rs`, `app/src/protocol.ts`,
`app/src/components/settings/ProvidersSection.tsx`,
`app/src/components/settings/agent-config.tsx`, and their Rust/TS fixtures.

This representation removes the invalid cross-product: a Claude value cannot
carry an endpoint, key, free-text selection, or removability; an OpenAI
compatible value cannot omit its endpoint/model fields; and detection/secret
status cannot combine mutually exclusive facts. It is separate from C13-01:
the union constrains shape, while the store parser must still reject empty
strings.

**Duplicate truth check.** No duplicate-truth write path was found. A
`ProviderInstance` is a derived outbound snapshot; it is not persisted and is
replaced wholesale in `ProviderRoster`. The provider selection in this roster
describes provider capability, while `ModelSnapshot`/`ForkAgentConfig` carry
the per-agent current selection; those are different dimensions, not a second
writer for the same stored fact.

**Existing and additional validation.** Existing Rust protocol fixtures cover
valid Claude and custom instances and a round trip
(`crates/hirsel-proto/src/tests.rs:576-624`); host tests cover valid Codex,
Claude, and custom snapshots (`crates/hirsel-host/src/providers.rs:470-501` and
`620-676`); TS fixtures likewise contain only valid variants
(`app/src/components/settings/settings-providers.test.tsx:21-59` and
`settings-agents.test.tsx:100-150`). None attempts an invalid cross-kind
combination. Add positive serialization tests for all three variants and
negative deserialization tests for the concrete Claude/custom mixtures,
detected/undetected status mixtures, and secret-status mixtures. Update the
settings tests to prove variant narrowing still preserves no-key leakage and
the existing main/fork controls. No tests were executed in this audit.

**Risk and confidence.** This is a real wire-shape cutover, not a local type
alias: current comments explicitly allow older hosts to omit `selection`
(`crates/hirsel-proto/src/providers.rs:85-88`) and the client has an older-host
fallback (`app/src/components/settings/agent-config.tsx:119-140`). The direct
cutover must update both ends and all fixtures together; retaining compatibility
would require an explicit boundary decoder and should not leak the old flat
shape into application state. Confidence: **medium** because the bad state is
latent rather than produced by the current host.

## Explicit coverage contract and skips

Every assigned whole-file owner was inspected. The status below distinguishes
the two findings from deliberate no-findings; consumers listed here were read
for conversion evidence only and are not treated as C13 owners.

| Owned file/definition | Result |
| --- | --- |
| `crates/hirsel-host/src/boot_provider.rs` | C13-01 boot consumer; no separate boot-plan finding. Missing key fallback is deliberate, but empty `base_url` is accepted because the store parser failed first. |
| `crates/hirsel-host/src/config.rs` | No finding. Environment/provider mode and API-key fields are a deliberate first-boot boundary; no second persisted truth was found. |
| `crates/hirsel-host/src/host_config/mod.rs` | No separate finding. The document/source cache, malformed-file fallback, prompt override provenance, and historical `[model].id` versus `[fork].model` keys are intentional; the provider parser is isolated in `provider_store.rs`. |
| `crates/hirsel-host/src/host_config/provider_store.rs` | C13-01 owner: read-side required-field validation gap. The lower raw-string agent writer was inspected and is covered by the shared `AgentSlot` skip below. |
| `crates/hirsel-host/src/lash_runtime/provider.rs` | C13-01 runtime consumer; no separate provider-construction finding. It passes the already parsed endpoint into the provider handle. |
| `crates/hirsel-host/src/model_selection.rs` | C13-01 empty-default consumer; no separate finding. Main/fork registries and free-text mode are intentional, and the selection fallback is explicit. |
| `crates/hirsel-host/src/prompt_config.rs` | No finding. Prompt effective text plus `is_default` is source/override provenance, not duplicate prompt truth; fork configuration follows the model surface. |
| `crates/hirsel-host/src/provider_detect.rs` | No finding. Constructors consistently emit detected-without-detail or undetected-with-detail and tests cover token non-leakage; the latent wire cross-product is included only in C13-02. |
| `crates/hirsel-host/src/providers.rs` | C13-01 writer/roster consumer and C13-02 producer evidence. Add/update validation is adequate; no separate roster-state duplicate truth was found. |
| `crates/hirsel-host/src/skills.rs` | No finding. Discovery precedence, canonical visited roots, depth bound, and `disable-model-invocation` behavior are deliberate; local invocation/lazy discovery is existing #7 and excluded. |
| `crates/hirsel-host/src/subagent_models.rs` | No finding. `enabled` plus `enabled_variants` intentionally preserves selected variants while a model is disabled; malformed config is rejected at the parser. |
| `crates/hirsel-proto/src/models.rs` | No finding. `ModelSnapshot`/`ForkAgentConfig` repeat the effective model surface for separate agents and retain older-host compatibility; `PromptDoc.is_default` records override provenance. |
| `crates/hirsel-proto/src/providers.rs` | C13-02 owner: flat provider wire state and nested status cross-products. |
| `prompts/agent.md` | No finding. Bundled instruction content is a prompt source, not a configuration representation. |
| `prompts/fork.md` | No finding. The one-event triage policy is content/policy, with no duplicate or invalid configuration state. |
| `crates/hirsel-host/src/storage/current.sql:148` — `thread_execution_preferences` | Inspected as the exact shared table. The behavioral Host-preference-to-global-default conversion is C02-01 in `/tmp/hirsel-combined-audit/workers/C02-TURN-LIFECYCLE.md`; reporting it here would duplicate that owner. |
| `crates/hirsel-proto/src/client.rs:22` — `AgentSlot` | Inspected. `Main`/`Fork` is already used at the public provider command and centralized by `section_for`/`model_key_for` (`providers.rs:185-197`, `327-340`). The lower store helper accepts raw strings, but all production call sites inspected pass the canonical pairs and no current wrong-key write path was found; not promoted to a third latent finding. |

The specified settings/protocol consumers were inspected for both findings,
including `app/src/protocol.ts`, `ProvidersSection.tsx`, `agent-config.tsx`,
`AgentsSection.tsx`, `ForkAgentSection.tsx`, the store reducer/types/selectors,
and the listed settings sections. No independent C14/C19 protocol or web
settings finding was claimed.

## Deduplication and audit conclusion

- C13-01 is a persisted TOML read/write invariant and does not overlap C02's
  execution-lifecycle conversion.
- C13-02 is an outbound representation/type-shape issue and has no storage
  writer; it does not collapse the intentionally independent Thread/model/prompt
  dimensions.
- The remaining leads were either deliberate denormalization/provenance,
  compatibility fields, or existing excluded outcomes. No third recommendation
  is carried forward.

Priority order: fix C13-01 at the `ConfigStore::providers()` boundary first;
consider C13-02 only as a coordinated protocol cutover with explicit negative
wire tests.

## Post-write verification

The required post-write check completed after this report was written:

```text
HEAD  3ee0621a603659ab0168f565b99012b642415419
tree  a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain  (empty)
git diff --stat  (empty)
git diff --name-only  (empty)
```

The report is outside the repository; repository source is unchanged and this
report is the only retained deliverable from the worker.
