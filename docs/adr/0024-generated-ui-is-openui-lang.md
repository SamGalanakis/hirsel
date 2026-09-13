# Generated UI is OpenUI Lang rendered natively

Accepted 2026-09-13.

An artifact whose purpose is to be *used* — a dashboard, a table, a set of
metrics, a form, a checklist — is published as `kind: "openui"`: one OpenUI Lang
v0.5 program written against a component vocabulary Hirsel owns, parsed by
`@openuidev/lang-core` and drawn by Hirsel's own Solid renderer in the host
document. It is a new kind beside `solid`, `html`, `markdown`, `image` and
`file`, not a replacement for any of them.

## The vocabulary rule

The library in `app/src/openui/schema.ts` is the entire surface a model may
generate. A component name the library does not define does not render; a
required prop the program omits drops that one statement and leaves the rest.
The same schemas generate the signatures the agent reads — `prompts/openui-library.md`
is produced from them by `app/scripts/openui-prompt.mjs` and `just check` fails
if it is stale — so the prompt and the renderer cannot disagree about what
exists. Because every renderer draws with the app's design tokens, generated UI
is on-brand by construction rather than by asking a model for restraint, and a
palette change reaches every artifact ever published without editing one.

This is what buys reliability from a weak model. The unit of correctness is a
line, not a file: `identifier = Component(...)`. A wrong line is dropped and
reported in a "n lines were dropped" disclosure; a wrong line in a JSX module is
a blank page. The same property makes editing cheap — `artifacts_edit` replaces
one statement — and makes progressive rendering real, because the streaming
parser yields a tree from whatever has arrived.

## Why not an iframe, and why not React

The existing `solid` kind compiles JSX in a worker and runs it inside a
sandboxed frame with no network, no same-origin access and no tool bridge. That
isolation is the right answer for code we did not write, and it costs what
isolation costs: a compile step, a frame that cannot inherit the app's theme or
type, a postMessage channel for every interaction, and a preview that fails
whole.

An `openui` body is not code. It has no expressions to evaluate against the
host, no imports and no way to reach anything: the worst a malformed program can
do is fail to describe a component. Nothing is gained by isolating data, and
everything the frame costs is paid back — the artifact renders with the app's
tokens in light and dark, keyboard navigation is ordinary DOM navigation, and an
interaction is a plain function call instead of a bridge.

React is not in this app and will not be added for this. Porting the ~1000-line
`svelte-lang` adapter shape to Solid was the smaller change, and it keeps one
reactive model in the codebase. lang-core itself is framework-agnostic and
stores each component's renderer opaquely, so the renderer signature is ours:
components receive their props, a `renderNode` for nested values, the render
context and their form scope, passed explicitly rather than through a Solid
context, so the tree can be built inside a memo without depending on the owner
chain.

## Actions are messages

A Button, a FollowUp or a Form submission sends one ordinary Owner message to
the artifact's Thread: the label the person saw, then a fenced JSON block with
`{artifact_id, action, params, form_state}`. There is no new wire op, no private
channel and no hidden state — what the Owner chose is in the conversation, where
the agent already reads, and the agent answers by editing the artifact. Form
values live in the render context rather than the DOM, so a body that is still
arriving, or an edit that arrives later, does not discard what was typed.

## Why the Solid kind stays

`openui` covers what the vocabulary covers. A custom visualisation, a canvas, an
animation, a simulation, a game — anything whose interest *is* the code — stays
`solid`, isolated in its frame. Adding those to the vocabulary would turn a
closed component set into an open one and give back the reliability this ADR was
adopted for. Markdown remains the document kind and HTML the self-contained page
kind.
