# Hirsel view catalog v1

This directory is the agent-editable source for Hirsel views. Each `*.json` file is a template with this envelope:

```json
{
  "id": "template-id",
  "title": "Human title",
  "params_schema": { "name": "string", "count": "number", "items": "array" },
  "spec": { "type": "text", "text": "Hello, {{name}}" }
}
```

`id` must match the filename and contain only ASCII letters, digits, `-`, or `_`. Supported parameter types are `string`, `number`, `boolean`, `array`, and `object`; every declared parameter is required. Undeclared parameters are allowed.

## Bindings

- `{{path}}` in a whole string preserves the bound JSON type. This is how numbers, booleans, arrays, and objects reach component props.
- Bindings embedded in text, such as `"{{files}} files changed"`, render strings directly and other values as compact JSON.
- Dot paths and numeric array segments are supported: `{{author.name}}`, `{{items.0.label}}`.
- A list expansion is one object inside an array: `{ "{{#each checks}}": { ... } }`. Within its template, bare paths and `{{this}}` refer to the current item. `{{@root.title}}` explicitly reads the root params.
- Missing bindings, type mismatches, malformed templates, and invalid component props reject the view before it reaches a client.

The host re-reads a template file every time it resolves that template. `HIRSEL_TEMPLATES_DIR` selects the directory; the default is `./templates` relative to the host working directory.

## Component shape

Every component is a JSON object with a required `type`. Unknown component types and unknown props are errors. Display scalars are strings, numbers, or booleans. Semantic tones are `default`, `muted`, `success`, `warning`, and `danger`; clients map them to the existing calm-terminal status tokens, never hard-coded colors.

### Layout and content

- `card`: optional `title`, optional `subtitle`, required `children: Component[]`.
- `stack`: optional `gap: "xs" | "sm" | "md" | "lg"`, required `children`.
- `row`: optional `gap`, optional `align: "start" | "center" | "end" | "stretch"`, optional `wrap: boolean`, required `children`.
- `heading`: required `text: string`, optional `level: 1..4` (default 2).
- `text`: required `text: display scalar`, optional semantic `tone`.
- `divider`: no props.

### Structured data

- `keyValue`: required `items: [{ label: string, value: display scalar, tone? }]`.
- `table`: required `columns: [{ key, label, align?: "start" | "center" | "end" }]`, required `rows: object[]`, optional `caption`. Every row must contain exactly the declared column keys and display-scalar values.
- `list`: required `items: [{ text: display scalar, tone? }]`, optional `ordered: boolean`.
- `checklist`: required `items: [{ label: string, checked: boolean, detail?: string }]`.

### Status

- `badge`: required `label`, optional semantic `tone`.
- `status`: required `label`, required `state: "neutral" | "running" | "success" | "warning" | "danger"`.
- `progress`: required `value: number` in `0..1`, optional `label`.
- `callout`: required `body`, optional `title`, optional `tone: "default" | "success" | "warning" | "danger"`.

### Interaction

- `action`: required `label` and `action`; optional `data` (any JSON) and `variant: "primary" | "secondary" | "danger"`. Activation emits `{ instance_id, action, data }`.
- `optionSet`: required `action` and `choices: [{ label, value: display scalar, description? }]`; optional `label` and `selected: display scalar`. Selection emits the declared action with `data: { "value": <choice value> }`.
- `field`: required `name`, `label`, and `kind: "text" | "textarea" | "number" | "toggle" | "select"`; optional scalar-or-null `value`, `placeholder`, and `required`. A select field requires `options: [{ label, value: display scalar }]`; other kinds reject `options`.
- `form`: required `action` and `fields: field[]`; optional `submitLabel`. Submission emits the declared action with `data` containing values keyed by field name.

The client must render actions as owner-initiated events and send a `view_event` frame. It must not execute commands or create a separate interaction channel.

## Deferred to v2

Images, markdown/rich text, charts, nested tables, conditional bindings, visibility expressions, client-side state actions, custom components, and executable content are intentionally outside v1.
