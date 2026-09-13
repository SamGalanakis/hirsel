/** The render context every OpenUI component renderer receives.
 *
 * lang-core stores a component's renderer opaquely, so the renderer signature
 * is ours to choose. It is passed explicitly rather than through a Solid
 * context so that the tree can be built inside a memo without depending on the
 * owner chain: one object, threaded down, no stale closures. */
import { createStore } from "solid-js";
import { validate, type ParsedRule } from "@openuidev/lang-core";
import type { JSX } from "@solidjs/web";

/** One field's recorded value plus the component that recorded it, so an
 * action payload says what kind of control produced each answer. */
export interface FieldEntry {
  value: unknown;
  componentType?: string;
}
/** Form state is namespaced by form name; fields outside a Form sit at the top
 * level under their own name, exactly as the lang-core action event expects. */
export type FormState = Record<string, FieldEntry | Record<string, FieldEntry>>;

/** What a triggered control hands back to the artifact surface. */
export interface OpenUiAction {
  /** The message a person reads: the button's own label. */
  message: string;
  /** Machine-readable action name the agent chose, if any. */
  action?: string;
  params: Record<string, unknown>;
  formName?: string;
  formState?: FormState;
}

export interface OpenUiContext {
  /** Values are read through this so a component's JSX tracks them. */
  fieldValue: (formName: string | undefined, name: string) => unknown;
  setFieldValue: (formName: string | undefined, componentType: string, name: string, value: unknown) => void;
  /** A field's current validation message, or undefined while it passes. */
  fieldError: (formName: string | undefined, name: string) => string | undefined;
  /** Declare a field as it is drawn: its rules, the control that owns it and
   * the value the program asked for. Registration is plain bookkeeping, so a
   * field never writes reactive state while it renders. */
  registerField: (field: { formName?: string; name: string; componentType: string; rules: ParsedRule[]; defaultValue?: unknown }) => void;
  /** Validate every registered field of one form. Returns whether it passed. */
  validateForm: (formName: string | undefined) => boolean;
  trigger: (action: OpenUiAction) => void;
  /** True while the body is still arriving: controls stay inert. */
  streaming: () => boolean;
}

/** A renderer receives its evaluated props, a way to draw nested values, the
 * shared context, and the name of the Form it sits inside, if any. */
export interface RenderProps<P = Record<string, unknown>> {
  props: P;
  /** Draw a nested prop value. A component that opens a naming scope — only
   * Form does — passes the scope its children belong to; everyone else
   * inherits the one they were rendered in. */
  renderNode: (value: unknown, formName?: string) => JSX.Element;
  ctx: OpenUiContext;
  formName?: string;
  statementId?: string;
}
export type OpenUiRenderer<P = Record<string, unknown>> = (props: RenderProps<P>) => JSX.Element;

/** Form and field names are agent-authored strings, so the key that joins a
 * field to its form uses a character neither can contain. */
const SEPARATOR = "\u0000";
const keyOf = (formName: string | undefined, name: string) => (formName ? `${formName}${SEPARATOR}${name}` : name);

interface Registration {
  formName?: string;
  name: string;
  componentType: string;
  rules: ParsedRule[];
  defaultValue?: unknown;
}

/** Build the mutable half of a render: form values, validation messages, and
 * the trigger that turns a control into one action for the caller. */
export function createOpenUiContext(options: {
  initialState?: FormState;
  streaming: () => boolean;
  onAction: (action: OpenUiAction) => void;
  onStateChange?: (state: FormState) => void;
}): OpenUiContext {
  const [values, setValues] = createStore<Record<string, FieldEntry>>(flatten(options.initialState));
  const [errors, setErrors] = createStore<Record<string, string | undefined>>({});
  const fields = new Map<string, Registration>();

  // A value the Owner has not touched falls back to the one the program
  // declared, so nothing has to be written into the store during a render.
  const fieldValue: OpenUiContext["fieldValue"] = (formName, name) => {
    const key = keyOf(formName, name);
    const entry = values[key];
    return entry ? entry.value : fields.get(key)?.defaultValue;
  };
  const check = (registration: Registration): string | undefined =>
    validate(fieldValue(registration.formName, registration.name), registration.rules);
  /** Everything currently drawn, whether it was typed into or left as declared. */
  const snapshot = (): FormState => {
    const state: FormState = {};
    for (const registration of fields.values()) {
      const entry: FieldEntry = { value: fieldValue(registration.formName, registration.name), componentType: registration.componentType };
      if (entry.value === undefined) continue;
      if (!registration.formName) { state[registration.name] = entry; continue; }
      const existing = state[registration.formName];
      const bucket = existing && !isEntry(existing) ? existing : {};
      bucket[registration.name] = entry;
      state[registration.formName] = bucket;
    }
    return state;
  };

  return {
    fieldValue,
    fieldError: (formName, name) => errors[keyOf(formName, name)],
    setFieldValue: (formName, componentType, name, value) => {
      const key = keyOf(formName, name);
      setValues(draft => { draft[key] = { value, componentType }; });
      const registration = fields.get(key);
      if (registration && errors[key] !== undefined) { const message = check(registration); setErrors(draft => { draft[key] = message; }); }
      options.onStateChange?.(snapshot());
    },
    registerField: field => { fields.set(keyOf(field.formName, field.name), field); },
    validateForm: formName => {
      let valid = true;
      for (const [key, registration] of fields) {
        if (registration.formName !== formName) continue;
        const message = check(registration);
        setErrors(draft => { draft[key] = message; });
        if (message) valid = false;
      }
      return valid;
    },
    trigger: action => options.onAction({ ...action, formState: scopedState(snapshot(), action.formName) }),
    streaming: options.streaming,
  };
}

/** The state an action reports: just the triggering form, or everything when
 * the control stands outside one. */
function scopedState(state: FormState, formName: string | undefined): FormState | undefined {
  if (formName) {
    const scoped = state[formName];
    return scoped ? ({ [formName]: scoped } as FormState) : undefined;
  }
  return Object.keys(state).length > 0 ? state : undefined;
}

function flatten(state: FormState | undefined): Record<string, FieldEntry> {
  const flat: Record<string, FieldEntry> = {};
  for (const [name, entry] of Object.entries(state ?? {})) {
    if (isEntry(entry)) flat[name] = entry;
    else for (const [field, value] of Object.entries(entry)) if (isEntry(value)) flat[keyOf(name, field)] = value;
  }
  return flat;
}
function isEntry(value: unknown): value is FieldEntry {
  return typeof value === "object" && value !== null && "value" in value;
}
