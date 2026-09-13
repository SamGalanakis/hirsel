/** Interactive half of the OpenUI vocabulary: one Form, native controls, and
 * the buttons that turn what was filled in into a single Owner message. */
import { createUniqueId, For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { parseRules } from "@openuidev/lang-core";
import { Button as UiButton } from "../../components/ui/button";
import { Input as UiInput } from "../../components/ui/input";
import { Textarea as UiTextarea } from "../../components/ui/textarea";
import type { RenderProps } from "../context";

/** Every control shares one shell: label above, message below, both optional
 * and both tied to the control for assistive technology. */
function Field(props: { label?: string; hint?: string; error?: string; id: string; children: JSX.Element }) {
  return <div class="flex min-w-0 flex-col gap-1">
    <Show when={props.label}><label for={props.id} class="text-meta font-medium text-muted-foreground">{props.label}</label></Show>
    {props.children}
    <Show when={props.error} fallback={<Show when={props.hint}><span class="text-meta text-muted-foreground">{props.hint}</span></Show>}>
      <span role="alert" class="text-meta text-status-danger">{props.error}</span>
    </Show>
  </div>;
}

interface FieldProps { name?: string; label?: string; hint?: string; rules?: unknown; value?: unknown }

/** Register the field's rules, seed its declared default once, and hand back
 * the reads and writes its control needs. Values live in the render context,
 * never in the DOM, so a re-parse during streaming keeps what was typed. */
function bind(p: RenderProps<FieldProps>, componentType: string, fallback?: unknown) {
  const name = p.props.name ?? componentType.toLowerCase();
  p.ctx.registerField({
    formName: p.formName, name, componentType,
    rules: parseRules(p.props.rules),
    defaultValue: p.props.value !== undefined ? p.props.value : fallback,
  });
  return {
    id: createUniqueId(),
    name,
    value: () => p.ctx.fieldValue(p.formName, name),
    text: () => { const value = p.ctx.fieldValue(p.formName, name); return value == null ? "" : String(value); },
    error: () => p.ctx.fieldError(p.formName, name),
    set: (value: unknown) => p.ctx.setFieldValue(p.formName, componentType, name, value),
    disabled: () => p.ctx.streaming(),
  };
}

export const Input = (p: RenderProps<FieldProps & { placeholder?: string; type?: string }>): JSX.Element => {
  const field = bind(p, "Input");
  return <Field id={field.id} label={p.props.label} hint={p.props.hint} error={field.error()}>
    <UiInput id={field.id} name={field.name} type={p.props.type ?? "text"} placeholder={p.props.placeholder}
      disabled={field.disabled()} aria-invalid={field.error() ? "true" : undefined}
      value={field.text()} onInput={event => field.set(event.currentTarget.value)} />
  </Field>;
};

export const Textarea = (p: RenderProps<FieldProps & { placeholder?: string; rows?: number }>): JSX.Element => {
  const field = bind(p, "Textarea");
  return <Field id={field.id} label={p.props.label} hint={p.props.hint} error={field.error()}>
    <UiTextarea id={field.id} name={field.name} placeholder={p.props.placeholder} rows={p.props.rows}
      disabled={field.disabled()} aria-invalid={field.error() ? "true" : undefined}
      value={field.text()} onInput={event => field.set(event.currentTarget.value)} />
  </Field>;
};

export const Select = (p: RenderProps<FieldProps & { options?: unknown; placeholder?: string }>): JSX.Element => {
  const field = bind(p, "Select");
  return <Field id={field.id} label={p.props.label} hint={p.props.hint} error={field.error()}>
    <select id={field.id} name={field.name} disabled={field.disabled()} aria-invalid={field.error() ? "true" : undefined}
      class="min-h-11 w-full min-w-0 rounded-lg border border-border bg-transparent px-2 text-sm text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
      value={field.text()} onChange={event => field.set(event.currentTarget.value)}>
      <option value="" disabled>{p.props.placeholder ?? "Choose…"}</option>
      <For each={options(p.props.options)}>{option => <option value={option}>{option}</option>}</For>
    </select>
  </Field>;
};

export const Checkbox = (p: RenderProps<FieldProps & { checked?: boolean }>): JSX.Element => {
  const field = bind(p, "Checkbox", p.props.checked ?? false);
  return <div class="flex min-w-0 flex-col gap-1">
    <label class="flex min-w-0 items-center gap-2 text-sm">
      <input type="checkbox" id={field.id} name={field.name} disabled={field.disabled()}
        class="size-4 accent-primary" checked={field.value() === true}
        onChange={event => field.set(event.currentTarget.checked)} />
      <span class="min-w-0 break-words">{p.props.label}</span>
    </label>
    <Show when={field.error()}><span role="alert" class="text-meta text-status-danger">{field.error()}</span></Show>
  </div>;
};

export const Radio = (p: RenderProps<FieldProps & { options?: unknown }>): JSX.Element => {
  const field = bind(p, "Radio");
  return <fieldset class="flex min-w-0 flex-col gap-1">
    <Show when={p.props.label}><legend class="text-meta font-medium text-muted-foreground">{p.props.label}</legend></Show>
    <For each={options(p.props.options)}>{option => <label class="flex min-w-0 items-center gap-2 text-sm">
      <input type="radio" name={`${p.formName ?? ""}-${field.name}`} value={option} disabled={field.disabled()}
        class="size-4 accent-primary" checked={field.value() === option} onChange={() => field.set(option)} />
      <span class="min-w-0 break-words">{option}</span>
    </label>}</For>
    <Show when={field.error()}><span role="alert" class="text-meta text-status-danger">{field.error()}</span></Show>
  </fieldset>;
};

export const Slider = (p: RenderProps<FieldProps & { min?: number; max?: number; step?: number }>): JSX.Element => {
  const min = () => p.props.min ?? 0;
  const field = bind(p, "Slider", p.props.min ?? 0);
  const current = () => (typeof field.value() === "number" ? (field.value() as number) : min());
  return <Field id={field.id} label={p.props.label} hint={p.props.hint} error={field.error()}>
    <div class="flex min-w-0 items-center gap-3">
      <input type="range" id={field.id} name={field.name} min={min()} max={p.props.max ?? 100} step={p.props.step ?? 1}
        disabled={field.disabled()} class="min-w-0 flex-1 accent-primary" value={current()}
        onInput={event => field.set(Number(event.currentTarget.value))} />
      <span class="shrink-0 text-meta tabular-nums text-muted-foreground">{current()}</span>
    </div>
  </Field>;
};

export const DatePicker = (p: RenderProps<FieldProps>): JSX.Element => {
  const field = bind(p, "DatePicker");
  return <Field id={field.id} label={p.props.label} hint={p.props.hint} error={field.error()}>
    <UiInput id={field.id} name={field.name} type="date" disabled={field.disabled()}
      aria-invalid={field.error() ? "true" : undefined}
      value={field.text()} onInput={event => field.set(event.currentTarget.value)} />
  </Field>;
};

const VARIANT: Record<string, "default" | "secondary" | "outline" | "ghost" | "destructive"> = {
  primary: "default", default: "default", secondary: "secondary", outline: "outline", ghost: "ghost", destructive: "destructive",
};
export const Button = (p: RenderProps<{ label?: string; action?: string; variant?: string; params?: unknown }>): JSX.Element => {
  const variant = () => VARIANT[p.props.variant ?? "default"] ?? "default";
  // A submitting button is the one that must not send an incomplete form; a
  // secondary one — cancel, skip, back — always gets through.
  const submits = () => variant() === "default" || variant() === "destructive";
  return <UiButton variant={variant()} size="sm" disabled={p.ctx.streaming()} class="min-h-11" onClick={() => {
    if (submits() && p.formName !== undefined && !p.ctx.validateForm(p.formName)) return;
    p.ctx.trigger({
      message: p.props.label ?? "Continue",
      action: p.props.action,
      params: isRecord(p.props.params) ? p.props.params : {},
      formName: p.formName,
    });
  }}>{p.props.label}</UiButton>;
};

/** The Form is a naming scope, not a `<form>`: submission is one action event,
 * never a browser navigation. Its name namespaces every field beneath it. */
export const Form = (p: RenderProps<{ name?: string; fields?: unknown; buttons?: unknown }>): JSX.Element => {
  const scope = () => p.props.name ?? "form";
  return <div role="form" aria-label={scope()} data-slot="openui-form" class="flex min-w-0 flex-col gap-4">
    {p.renderNode(p.props.fields, scope())}
    <div class="flex flex-wrap gap-2">{p.renderNode(p.props.buttons, scope())}</div>
  </div>;
};

function options(value: unknown): string[] {
  return Array.isArray(value) ? value.map(item => (item == null ? "" : String(item))) : [];
}
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
