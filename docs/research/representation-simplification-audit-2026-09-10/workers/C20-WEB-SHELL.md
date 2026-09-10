# C20-WEB-SHELL audit

## Findings

### 1. Recommend — `PaneHeader` silently drops a live saving indicator

**Verdict:** Recommend. This is a reachable invalid component state, not a
style-only concern.

**Evidence and condition.** The owned props describe `badge` as an accessory
for a non-dismissible header, while `onClose` is independently optional:

> `app/src/components/ui/PaneHeader.tsx:23-35`
>
> `interface Props {`
> `  icon: JSX.Element;`
> `  title: string;`
> `  titleId?: string;`
> `  onClose?: () => void;`
> `  closeLabel?: string;`
> `  /** A trailing accessory for a non-dismissible pane. */`
> `  badge?: JSX.Element;`
> `}`

The renderer treats those two fields as an exclusive choice, with the close
branch taking precedence:

> `app/src/components/ui/PaneHeader.tsx:65-74`
>
> `<Show when={props.onClose} fallback={props.badge}>`
> `  <button ... aria-label={props.closeLabel ?? "Close"} ...>`
> `    ...`
> `  </button>`
> `</Show>`

The current prompt editor supplies both values on its reachable expanded
editor path:

> `app/src/components/settings/agent-config.tsx:445-456`
>
> `<PaneHeader`
> `  ...`
> `  onClose={props.onClose}`
> `  closeLabel={`Close ${props.label}`}`
> `  ...`
> `  badge={`
> `    <Show when={props.busy()}>`
> `      <LoaderCircle ... aria-label="Saving" />`
> `    </Show>`
> `  }`
> `/>`

When `busy()` is true during a prompt save, `onClose` is truthy and the
fallback is never rendered; the `Saving` loader is therefore absent. This is a
reachable write/prop path, not a latent caller mistake. No duplicate-truth
write path was found: the defect is an unsupported/incorrectly modelled
combination of two UI capabilities.

**Consumer blast radius.**

```text
rg -n '<PaneHeader' app/src --glob '!**/pane-header.test.tsx'
```

returns **4** production consumers (`ProcessesSheet`, `CanvasSurface`,
`agent-config`, and `SettingsSheet`). The conflicting consumer is confirmed by:

```text
rg -n 'onClose=|badge=' app/src/components/settings/agent-config.tsx
```

which returns **2** relevant prop lines at 449 and 452. The current owned tests
cover close-only and badge-only cases (`app/src/components/ui/pane-header.test.tsx:12-45`),
but no fixture supplies both, so they do not demonstrate the condition.

**Target representation.** The actual product state has two independent
trailing capabilities: `close?: { onClose: () => void; label?: string }` and
`badge?: JSX.Element`; both, either, or neither can be present. Keep those as
independent props (or make one explicit `trailing` node plus an independent
close prop), render the badge and close control as separate siblings, and
change the stale “non-dismissible” contract. Do not use an if/else fallback.
There is no wire, storage, or database layer for this component.

**Smallest credible scope:** `app/src/components/ui/PaneHeader.tsx` and
`app/src/components/ui/pane-header.test.tsx`; `agent-config.tsx` remains the
existing consumer. Add a both-present regression asserting the Saving label
and the labelled close control coexist. Existing close-parity and badge-only
assertions should remain. No tests were run.

**Risk and confidence:** Low cutover risk: existing close-only and badge-only
callers retain their output; the only intentional change is making the current
prompt-editor loader visible. A possible layout-width change is the expected
tradeoff for showing the already-requested status. **Confidence: high.**

### 2. Recommend — theme bootstrap and runtime have divergent owners for the same metadata

**Verdict:** Recommend. The browser theme is a valid `ThemeMode` state, but its
`theme-color` projection and invalid-value handling are duplicated across two
layers and have already drifted.

**Evidence and condition.** The pre-paint HTML path starts from one literal and
writes a second pair of literals:

> `app/index.html:13,22-29`
>
> `<meta name="theme-color" content="#141414" />`
> `var mode = localStorage.getItem("hirsel.theme") || "system";`
> `...`
> `if (meta) meta.setAttribute("content", dark ? "#141414" : "#fafafb");`

The runtime declares the same persisted union but uses a different pair:

> `app/src/lib/theme.ts:15-21`
>
> `export type ThemeMode = "system" | "light" | "dark";`
> `const STORAGE_KEY = "hirsel.theme";`
> `const DARK_META = "#162126";`
> `const LIGHT_META = "#f5faf7";`

and writes those values from both a user selection and a System-mode OS
transition:

> `app/src/lib/theme.ts:45-53,58-71`
>
> `document.querySelector('meta[name="theme-color"]')`
> `  ?.setAttribute("content", dark ? DARK_META : LIGHT_META);`
> `if (themeMode() === "system") applyTheme("system");`
> `...`
> `applyTheme(mode);`

Thus a stored/effective `dark` state can expose `#141414` during bootstrap and
`#162126` after the next runtime apply; `light` similarly changes from
`#fafafb` to `#f5faf7`. `git blame` shows the runtime pair was changed in
`4a0ca43f` while the bootstrap pair remained from `4db6326f`, demonstrating an
actual write-path change to one owner without the other. The comments in
`theme.ts:18-19` still claim the old values and “Mirrors index.html”.

There is a second layer-drift state: `theme.ts:23-30` validates persisted input
against the union and maps an invalid raw value to `"system"`, while the
bootstrap script only treats exact `"dark"` and `"system"` specially. A raw
invalid value therefore bootstraps as light even when the runtime's System mode
should follow a dark OS setting. Normal app writes use the union, so this is
latent unless local storage is stale/corrupt or externally edited; the color
drift is reachable through ordinary theme changes and OS transitions. No live
storage values, rows, or fixtures were read.

**Duplicate-truth write path.** This finding explicitly has one: the inline
bootstrap writes `meta.content` before the bundle, while `setThemeMode` and the
System media-query listener write the same DOM property later. The two source
copies can and did change independently. The `.dark` class and `ThemeMode`
signal are derived/authoritative state, not an additional finding.

**Consumer blast radius.**

```text
rg -n 'theme-color|DARK_META|LIGHT_META' app/index.html app/src/lib/theme.ts
```

returns **7** shape/write lines. Runtime consumers are found by:

```text
rg -n '\b(themeMode|setThemeMode)\b' app/src --glob '!**/theme.ts'
```

which returns **5** lines across `AppearanceSection` and `SettingsSheet`.
The focused test query

```text
rg -n 'theme-color|DARK_META|LIGHT_META|setThemeMode' app/src --glob '*test*'
```

returns **0** lines; the existing Settings tests only demonstrate that a Theme
control is present. No test or fixture demonstrates bootstrap/runtime parity.

**Target representation.** Keep the persisted `ThemeMode` union and its
validation in the runtime contract. Make one static HTML theme-color record the
owner of the two effective colors, for example the `theme-color` meta's
`data-light` and `data-dark` values with a light default `content`. Both the
pre-paint script and `applyTheme` should validate the same three-mode input and
read those attributes; remove `DARK_META`/`LIGHT_META` literals from TypeScript.
The effective state remains `{ mode: "system" | "light" | "dark", dark: boolean
derived from mode + prefers-color-scheme }`; no wire, table, or storage-schema
change is required.

**Smallest credible scope:** `app/index.html`, `app/src/lib/theme.ts`, and a
new focused theme test/HTML bootstrap assertion. Verify explicit Light/Dark,
System plus media-query change, and an invalid stored string all produce one
consistent meta value and class. No tests or builds were run.

**Risk and confidence:** Low-to-medium risk: the metadata source becomes a
DOM contract used during early boot, so missing/malformed attributes need a
safe default; the existing three user modes and CSS class semantics stay
unchanged. The benefit is deleting two independent color mappings and one
unvalidated bootstrap parser. **Confidence: high.**

## Cross-cutting patterns and deduplication

No additional cross-cutting pattern was promoted. The two recommendations are
independent: one is a local component capability composition; the other is the
HTML/runtime theme projection. The supplied exclusions were checked, including
the existing thread/artifact/driver outcomes. Adjacent worker findings for
artifact CSP/source-mode state, Related links, and connection/auth lifecycle
are distinct and remain with their owning clusters.

## Coverage contract and skip log

All 60 exact whole-file owners in `C20-WEB-SHELL.md` were read at the current
snapshot. There are no shared exact owned definitions. The following inventory
records the complete boundary, inspected symbols, and disposition.

| Cluster | Exact owned files and inspected definitions | Status / skip reasoning |
| --- | --- | --- |
| C20-BOOT | `app/index.html` (head metadata, CSP, pre-paint theme script); `app/src/main.tsx` (root lookup, render, service-worker registration); `app/src/App.tsx` (`initialToken`, `App` auth/plugin/title/favicon/notification effects); `app/src/components/TokenGate.tsx` (`Props`, `TokenGate`); `app/src/components/BrandMark.tsx` (`BrandMark`) | Recommend F2 owns the bootstrap/theme portion. Root/token/plugin and brand rendering skip: state ownership is clear, auth rejection is covered by `app-auth.test.tsx`, and brand tokenization is covered by `brand-mark.test.tsx`. |
| C20-UI | `app/src/components/CommandPalette.tsx` (`Command`, `fuzzyMatch`, `CommandPalette`, `ShortcutHelp`, `KeyHint`, `ModalSurface`, `ModalPanel`); `ConnectionPill.tsx` (`LABEL`, `connectionLabel`, `ConnectionPill`); `Toaster.tsx` (`Toaster`); `ui/PaneHeader.tsx` (`Props`, `PaneHeader`); `ui/badge.tsx` (`badgeVariants`, `BadgeProps`, `Badge`); `button.tsx` (`buttonVariants`, `ButtonProps`, `Button`); `card.tsx` (`Card*` props/components); `dropdown-menu.tsx` (`MenuState`, context, menu helpers and all exported parts); `empty.tsx` (`Empty*` props/components); `icons.tsx` (`IconProps`, `IconNode`, `Icon`, all icon constants); `input.tsx` (`InputProps`, `Input`); `textarea.tsx` (`TextareaProps`, `Textarea`) | Recommend only the PaneHeader combination in F1. Palette IDs/hints, connection labels, toast queue rendering, primitive variants, dropdown state, empty-state composition, SVG node table, and input/textarea pass-throughs were inspected and skipped: no materially useful invalid state or duplicate owner was established. `EmptyDescription`'s paragraph props/div output was considered but is a narrow semantic mismatch with one current consumer, not a useful representation/control-flow finding for this capped audit. |
| C20-MARKDOWN | `components/Markdown.tsx` (link-definition context, reference/image/link rendering, phrasing/block renderers, `Markdown`, `renderInline`, `stripInlineMarkdown`); `components/markdown/CodeBlock.tsx` (`hastClass`, `renderHast`, `CopyButton`, `CodeBlock`); `highlight.ts` (`grammars`, `GrammarName`, `aliases`, `resolveLanguage`, lazy lowlight state, `highlight`); `parse.ts` (extensions, `parseMarkdown`, `healStreamingMarkdown`, `parseStreamingMarkdown`); `url.ts` (`SAFE_SCHEMES`, `safeHref`) | Skip: the mdast-to-JSX boundary is explicit, raw HTML is rendered as text, URL schemes are checked, highlighting declines unknown languages, and owned Markdown/highlight tests cover these behaviors. No new representation defect was accepted; adjacent artifact Markdown remains with C16. |
| C20-PREFS-THEME | `app/src/lib/prefs.ts` (local keys, boolean readers/signals/setters); `app/src/lib/theme.ts` (`ThemeMode`, storage reader, effective-mode resolver, theme apply/setter); `app/src/components/settings` theme consumers were read as consumers only | Recommend F2 for theme projection. Boolean preferences are validated at their local-storage boundary and remain browser-local; no duplicate key or invalid combination was found. |
| C20-BROWSER | `lib/caret.ts` (`MIRRORED`, `CaretPoint`, `caretPoint`, `hyphenate`); `clipboard.ts` (copy helper and `copyWithToast`); `focus.ts` (media flag, focus handoff, trap/presence state and helpers); `format.ts` (byte/time/snippet/file helpers); `keymap.ts` (`PaneTarget`, overlay signals, actions, `Shortcut`, `SHORTCUTS`, handlers, editable check, chord map, installer); `pwa.ts` (`registerServiceWorker`); `scroll.ts` (`ScrollGeometry`, threshold and scroll decisions); `submitKeymap.ts` (`SubmitKeymapHandlers`, `handleSubmitKeys`); `toast.ts` (`ToastVariant`, `ToastAction`, `Toast`, `Countdown`, queue/timer operations); `utils.ts` (`cn`); `version.ts` (`APP_VERSION`) | Skip: browser fallbacks, focus/presence ownership, keyboard state, scroll geometry, submit keymap, timer countdown, and PWA lifecycle are explicit and covered by their owned focused tests where present. Repeated display vocabulary and shortcut hints are small/intentional here and did not meet the materiality threshold. |
| C20-STYLES-ASSETS | `app/src/styles.css` (theme tokens, breakpoints, base/PWA/scrollbar/motion/highlight rules); `styles/shadcn-scroll-shimmer.css` (all vendored scroll-fade/shimmer properties/utilities); `styles/vega.css` (badge/button/card/empty/input/label/separator/skeleton/textarea/attachment classes); `app/public/favicon-dot.svg`, `favicon.svg`, `icon.svg` (static cube assets); `robots.txt` (disallow rule) | Recommend F2 only for the HTML/runtime metadata projection. CSS token blocks, vendored utility families, Vega primitive classes, static favicon variants, and robots policy have no separately owned invalid state or duplicate truth accepted. Static asset color variants are deliberate per asset/theme behavior. |
| C20-TESTS | `app-auth.test.tsx`, `attention.test.tsx`, `components/CommandPalette.test.tsx`, `components/Markdown.test.tsx`, `components/brand-mark.test.tsx`, `components/markdown/highlight.test.ts`, `components/ui/dropdown-menu.test.tsx`, `components/ui/pane-header.test.tsx`, `lib/endpoint.test.ts`, `lib/focus.test.ts`, `lib/keymap.test.ts`, `lib/overlay-presence.test.tsx`, `lib/scroll.test.ts`, `lib/submitKeymap.test.ts`, `lib/thread-ref.test.ts`, `lib/thread-url.test.ts`, `lib/toast.test.ts`, `mock-server.contract.test.ts` (all suites and fixtures) | Coverage/validation layer inspected. Existing tests establish the listed normal contracts; PaneHeader lacks the both-present case and theme has no focused parity case. No tests were executed by instruction. |

## Audit log and source-state verification

- Read the assigned specification, exclusions, repository instructions, and the
  invoked `schemasmash` and `audit-your-codebase` instructions.
- Baseline command: `git rev-parse HEAD HEAD^{tree}; git status --porcelain`.
  Expected and observed HEAD: `3ee0621a603659ab0168f565b99012b642415419`.
  Expected and observed tree: `a4aac830c45398a66591f2c44b707aaf3cef281b`.
  The porcelain output was empty.
- Read all owned definitions and bounded consumer/test context with `rg`,
  `nl`, `sed`, and targeted `git blame/show`. No application code, tests,
  builds, installs, migrations, providers, live data/config, sessions, or
  processes were run or changed.
- Re-opened every accepted finding and re-ran its consumer queries. No finding
  outside the two above survived the materiality, ownership, or deduplication
  checks.
- Post-report command `git rev-parse HEAD HEAD^{tree}; git status --porcelain`
  returned the same HEAD `3ee0621a603659ab0168f565b99012b642415419`, the same
  tree `a4aac830c45398a66591f2c44b707aaf3cef281b`, and empty porcelain output.

Fix F1 first: it is an existing reachable UI defect with a minimal local
cutover, while F2 is broader bootstrap/runtime contract cleanup.
