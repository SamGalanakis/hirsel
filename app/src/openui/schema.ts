/** Hirsel's OpenUI component vocabulary — the schemas alone.
 *
 * This is the whole surface a model may generate: a name it cannot find here
 * does not render, so generated UI is on-brand by construction rather than by
 * asking for restraint. The schemas are also the prompt — the signature an
 * agent reads is derived from them, never written twice — so this file stays
 * free of JSX and of the app's module aliases and can be imported by the
 * prompt build script directly.
 *
 * Each component's `component` field holds its own name; `library.ts` swaps in
 * the real renderer. lang-core never inspects that field. */
import { z } from "zod/v4";
import { createLibrary, defineComponent, tagSchemaId, type LibrarySpec } from "@openuidev/lang-core";

/** Any component from this library. Tagged so prompt signatures read
 * `children: Component[]` instead of `children: any[]`. */
const Component = z.any();
tagSchemaId(Component, "Component");
const Children = z.array(Component);

const define = <T extends z.ZodObject>(config: { name: string; props: T; description: string }) =>
  defineComponent<T, string>({ ...config, component: config.name });

const TabItem = define({
  name: "TabItem",
  props: z.object({ value: z.string(), trigger: z.string(), content: Children }),
  description: "One tab: a stable value, the label on its trigger, and the components in its panel.",
});

const ListItem = define({
  name: "ListItem",
  props: z.object({ text: z.string(), detail: z.string().optional() }),
  description: "One list row. `detail` is right-aligned meta text such as a count or a date.",
});

export const COMPONENT_SCHEMAS = [
  define({
    name: "Stack",
    props: z.object({
      children: Children,
      direction: z.enum(["col", "row"]).optional(),
      gap: z.enum(["none", "sm", "md", "lg"]).optional(),
      align: z.enum(["start", "center", "end", "stretch"]).optional(),
      justify: z.enum(["start", "center", "end", "between"]).optional(),
      wrap: z.boolean().optional(),
    }),
    description: "Layout container and the root of every program. Stack a column of sections, or a wrapping row of metrics and cards.",
  }),
  define({
    name: "Section",
    props: z.object({ title: z.string(), children: Children, description: z.string().optional() }),
    description: "A titled group of related content, with an optional one-line description under the title.",
  }),
  define({
    name: "Heading",
    props: z.object({ text: z.string(), level: z.number().optional() }),
    description: "A standalone heading, level 1 to 3. Prefer Section when the heading introduces a group.",
  }),
  define({
    name: "Text",
    props: z.object({ text: z.string(), tone: z.enum(["default", "muted"]).optional(), size: z.enum(["sm", "md"]).optional() }),
    description: "One paragraph of plain text. Use Markdown when the prose needs lists, emphasis or links.",
  }),
  define({
    name: "Markdown",
    props: z.object({ text: z.string() }),
    description: "CommonMark/GFM prose rendered through the app's own parser. Use for anything longer than a paragraph.",
  }),
  define({
    name: "Metric",
    props: z.object({ label: z.string(), value: z.string(), delta: z.string().optional(), trend: z.enum(["up", "down", "flat"]).optional() }),
    description: "One headline number with its label and optional change. Put several in a row Stack to make a metrics band.",
  }),
  define({
    name: "Card",
    props: z.object({ children: Children, title: z.string().optional(), description: z.string().optional() }),
    description: "A bordered panel grouping components under an optional title.",
  }),
  define({
    name: "Callout",
    props: z.object({ text: z.string(), variant: z.enum(["info", "success", "warning", "danger"]).optional(), title: z.string().optional() }),
    description: "A short highlighted notice. Use danger only for something that actually went wrong.",
  }),
  define({
    name: "Table",
    props: z.object({ columns: z.array(z.string()), rows: z.array(z.array(z.string())), caption: z.string().optional() }),
    description: "Row-oriented data table. `rows` holds one array of cell strings per row, in column order.",
  }),
  define({
    name: "List",
    props: z.object({ items: z.array(ListItem.ref), ordered: z.boolean().optional() }),
    description: "A bulleted or numbered list of ListItem rows.",
  }),
  ListItem,
  define({
    name: "Image",
    props: z.object({ src: z.string(), alt: z.string(), caption: z.string().optional() }),
    description: "An image by URL or data URI. `alt` is required and must describe the picture.",
  }),
  define({
    name: "Separator",
    props: z.object({}),
    description: "A horizontal rule between groups.",
  }),
  define({
    name: "Tabs",
    props: z.object({ items: z.array(TabItem.ref) }),
    description: "Tabbed panels. Only the selected panel is drawn.",
  }),
  TabItem,
  define({
    name: "CodeBlock",
    props: z.object({ code: z.string(), language: z.string().optional() }),
    description: "Preformatted source or output in the monospace scale.",
  }),
  define({
    name: "Chart",
    props: z.object({
      variant: z.enum(["bar", "line", "pie"]),
      labels: z.array(z.string()),
      values: z.array(z.number()),
      title: z.string().optional(),
    }),
    description: "A single-series bar, line or pie chart. `labels` and `values` are parallel arrays.",
  }),
  define({
    name: "Form",
    props: z.object({ name: z.string(), fields: Children, buttons: Children }),
    description: "Groups input components under one name and the buttons that submit them. Every field inside is reported under this name.",
  }),
  define({
    name: "Input",
    props: z.object({
      name: z.string(),
      label: z.string().optional(),
      placeholder: z.string().optional(),
      type: z.enum(["text", "email", "number", "password", "tel", "url"]).optional(),
      rules: z.array(z.string()).optional(),
      value: z.string().optional(),
      hint: z.string().optional(),
    }),
    description: 'One-line text field. `rules` are validation strings such as "required", "email", "minLength:3".',
  }),
  define({
    name: "Textarea",
    props: z.object({
      name: z.string(), label: z.string().optional(), placeholder: z.string().optional(),
      rules: z.array(z.string()).optional(), value: z.string().optional(), hint: z.string().optional(), rows: z.number().optional(),
    }),
    description: "Multi-line text field for longer answers.",
  }),
  define({
    name: "Select",
    props: z.object({
      name: z.string(), options: z.array(z.string()), label: z.string().optional(), placeholder: z.string().optional(),
      rules: z.array(z.string()).optional(), value: z.string().optional(), hint: z.string().optional(),
    }),
    description: "Single choice from a list. Use Radio instead when there are three or fewer options worth showing at once.",
  }),
  define({
    name: "Checkbox",
    props: z.object({ name: z.string(), label: z.string(), checked: z.boolean().optional() }),
    description: "One boolean answer.",
  }),
  define({
    name: "Radio",
    props: z.object({
      name: z.string(), options: z.array(z.string()), label: z.string().optional(),
      rules: z.array(z.string()).optional(), value: z.string().optional(),
    }),
    description: "Single choice shown as a group of radio buttons.",
  }),
  define({
    name: "Slider",
    props: z.object({
      name: z.string(), label: z.string().optional(), min: z.number().optional(), max: z.number().optional(),
      step: z.number().optional(), value: z.number().optional(), hint: z.string().optional(),
    }),
    description: "A number chosen from a range.",
  }),
  define({
    name: "DatePicker",
    props: z.object({
      name: z.string(), label: z.string().optional(), rules: z.array(z.string()).optional(),
      value: z.string().optional(), hint: z.string().optional(),
    }),
    description: "A date field. Values are ISO `YYYY-MM-DD` strings.",
  }),
  define({
    name: "Button",
    props: z.object({
      label: z.string(),
      action: z.string().optional(),
      variant: z.enum(["primary", "secondary", "outline", "ghost", "destructive"]).optional(),
      params: z.record(z.string(), z.any()).optional(),
    }),
    description: "Sends one message back to this Thread carrying `action`, `params` and, inside a Form, everything that form holds. A primary button validates its form first.",
  }),
  define({
    name: "FollowUp",
    props: z.object({ suggestions: z.array(z.string()) }),
    description: "Suggested replies. Choosing one sends it to this Thread as an ordinary message.",
  }),
];

export const ROOT = "Stack";
export const LIBRARY_ID = "hirsel";
export const COMPONENT_GROUPS = [
  { name: "Layout", components: ["Stack", "Section", "Card", "Separator", "Tabs", "TabItem"] },
  { name: "Content", components: ["Heading", "Text", "Markdown", "Metric", "Callout", "Table", "List", "ListItem", "Image", "CodeBlock", "Chart"] },
  {
    name: "Interaction",
    components: ["Form", "Input", "Textarea", "Select", "Checkbox", "Radio", "Slider", "DatePicker", "Button", "FollowUp"],
    notes: ["Every input needs a unique `name`. Put inputs inside a Form so their answers arrive together."],
  },
];

/** The library spec the prompt generator reads. Renderers are irrelevant to it. */
export function promptSpec(): LibrarySpec {
  return createLibrary<string>({
    id: LIBRARY_ID,
    root: ROOT,
    components: COMPONENT_SCHEMAS,
    componentGroups: COMPONENT_GROUPS,
  }).toSpec();
}
