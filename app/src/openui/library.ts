/** The renderable library: the shared schemas from `schema.ts` with this app's
 * Solid renderers attached. The vocabulary is defined once, beside the prompt
 * the agent reads, so a component can never exist in the prompt and not on the
 * screen — or the reverse. */
import { createLibrary, type Library } from "@openuidev/lang-core";
import * as blocks from "./components/blocks";
import * as fields from "./components/fields";
import { Chart } from "./components/chart";
import { COMPONENT_GROUPS, COMPONENT_SCHEMAS, LIBRARY_ID, ROOT } from "./schema";
import type { OpenUiRenderer } from "./context";

const RENDERERS: Record<string, OpenUiRenderer<never>> = {
  Stack: blocks.Stack, Section: blocks.Section, Heading: blocks.Heading, Text: blocks.Text,
  Markdown: blocks.Markdown, Metric: blocks.Metric, Card: blocks.Card, Callout: blocks.Callout,
  Table: blocks.Table, List: blocks.List, ListItem: blocks.ListItem, Image: blocks.Image,
  Separator: blocks.Separator, Tabs: blocks.Tabs, TabItem: blocks.TabItem, CodeBlock: blocks.CodeBlock,
  Chart, FollowUp: blocks.FollowUp,
  Form: fields.Form, Input: fields.Input, Textarea: fields.Textarea, Select: fields.Select,
  Checkbox: fields.Checkbox, Radio: fields.Radio, Slider: fields.Slider, DatePicker: fields.DatePicker,
  Button: fields.Button,
} as Record<string, OpenUiRenderer<never>>;

export type OpenUiLibrary = Library<OpenUiRenderer<never>>;

/** The one library instance the renderer uses. A schema without a renderer is
 * a programming error, not a runtime condition: it fails the build here. */
export const hirselLibrary: OpenUiLibrary = createLibrary<OpenUiRenderer<never>>({
  id: LIBRARY_ID,
  root: ROOT,
  componentGroups: COMPONENT_GROUPS,
  components: COMPONENT_SCHEMAS.map(component => {
    const renderer = RENDERERS[component.name];
    if (!renderer) throw new Error(`OpenUI component ${component.name} has a schema but no renderer.`);
    return { ...component, component: renderer };
  }),
});
