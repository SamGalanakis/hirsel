use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::json_spec::{
    SEMANTIC_TONES, STATUS_STATES, allowed, optional_bool, optional_enum, optional_integer_range,
    optional_string, required_display_scalar, required_enum, required_string,
};

const MAX_NODES: usize = 128;
const MAX_DEPTH: usize = 8;
pub const MAX_ACTION_DATA_BYTES: usize = 8 * 1024;
const MAX_ACTIONS: usize = 16;
const MAX_FIELDS: usize = 16;
const MAX_FIELD_STRING_BYTES: usize = 1024;
const MAX_CHOICES: usize = 16;
const MAX_ACTION_NAME_BYTES: usize = 64;

/// Validate the closed, semantic Task-instrument vocabulary. Clients may
/// degrade unknown historical nodes, but producer-authored recompositions are
/// accepted only after this Host-side boundary succeeds.
pub fn validate(ui: &Value) -> anyhow::Result<()> {
    let mut nodes = 0;
    match ui {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                validate_node(item, &format!("ui[{index}]"), 0, &mut nodes)?;
            }
        }
        _ => validate_node(ui, "ui", 0, &mut nodes)?,
    }
    if nodes == 0 {
        anyhow::bail!("Task UI must contain at least one component");
    }
    Contract::from_ui(ui)?;
    Ok(())
}

/// Return whether `action` is declared by the current Task UI and whether its
/// node explicitly marks it non-settling. This prevents a client from
/// inventing producer actions that were not rendered authoritatively.
pub fn action_contract(ui: &Value, action: &str) -> Option<bool> {
    Contract::from_ui(ui)
        .ok()?
        .actions
        .get(action)
        .map(|spec| spec.settles)
}

/// Validate the payload for an action declared by the current instrument and
/// return the authoritative action intent derived from that declaration.
pub fn validate_action(ui: &Value, action: &str, data: &Value) -> anyhow::Result<ValidatedAction> {
    validate(ui)?;
    if serde_json::to_vec(data)?.len() > MAX_ACTION_DATA_BYTES {
        anyhow::bail!("Task action data exceeds {MAX_ACTION_DATA_BYTES} bytes");
    }
    let object = data
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Task action data must be an object"))?;
    let contract = Contract::from_ui(ui)?;
    let spec = contract.actions.get(action).ok_or_else(|| {
        anyhow::anyhow!("action `{action}` is not declared by the current Task UI")
    })?;
    let choice_label = match &spec.kind {
        ActionKind::Options(options) => Some(validate_option_payload(object, options)?.to_string()),
        ActionKind::Form => {
            validate_form_payload(object, &contract.fields)?;
            None
        }
    };
    Ok(ValidatedAction {
        settles: spec.settles,
        choice_label,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct ValidatedAction {
    pub settles: bool,
    pub choice_label: Option<String>,
}

#[derive(Debug)]
struct Contract {
    actions: HashMap<String, ActionSpec>,
    fields: HashMap<String, FieldSpec>,
}

#[derive(Debug)]
struct ActionSpec {
    settles: bool,
    kind: ActionKind,
}

#[derive(Debug)]
enum ActionKind {
    Options(HashMap<String, String>),
    Form,
}

#[derive(Debug)]
struct FieldSpec {
    required: bool,
}

impl Contract {
    fn from_ui(ui: &Value) -> anyhow::Result<Self> {
        let mut contract = Self {
            actions: HashMap::new(),
            fields: HashMap::new(),
        };
        let mut stack = match ui {
            Value::Array(items) => items.iter().collect::<Vec<_>>(),
            value => vec![value],
        };
        while let Some(node) = stack.pop() {
            let Some(object) = node.as_object() else {
                continue;
            };
            match object.get("type").and_then(Value::as_str) {
                Some("optionList") => {
                    let action = object
                        .get("action")
                        .and_then(Value::as_str)
                        .unwrap_or("choose");
                    let mut options = HashMap::new();
                    for option in object
                        .get("options")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let key = option
                            .get("key")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let label = option
                            .get("label")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        options.insert(key.to_string(), label.to_string());
                    }
                    contract.insert_action(
                        action,
                        object.get("settles").and_then(Value::as_bool) != Some(false),
                        ActionKind::Options(options),
                    )?;
                }
                Some("submit") => {
                    let action = object
                        .get("action")
                        .and_then(Value::as_str)
                        .unwrap_or("submit");
                    contract.insert_action(
                        action,
                        object.get("settles").and_then(Value::as_bool) != Some(false),
                        ActionKind::Form,
                    )?;
                }
                Some("field") => {
                    let name = object
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if contract.fields.len() >= MAX_FIELDS {
                        anyhow::bail!("Task UI exceeds {MAX_FIELDS} fields");
                    }
                    if contract
                        .fields
                        .insert(
                            name.to_string(),
                            FieldSpec {
                                required: object
                                    .get("required")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                            },
                        )
                        .is_some()
                    {
                        anyhow::bail!("duplicate Task field name `{name}`");
                    }
                }
                _ => {}
            }
            if let Some(children) = object.get("children").and_then(Value::as_array) {
                stack.extend(children);
            }
        }
        Ok(contract)
    }

    fn insert_action(
        &mut self,
        action: &str,
        settles: bool,
        kind: ActionKind,
    ) -> anyhow::Result<()> {
        if action.len() > MAX_ACTION_NAME_BYTES {
            anyhow::bail!("Task action name exceeds {MAX_ACTION_NAME_BYTES} bytes");
        }
        if self.actions.len() >= MAX_ACTIONS {
            anyhow::bail!("Task UI exceeds {MAX_ACTIONS} actions");
        }
        if self
            .actions
            .insert(action.to_string(), ActionSpec { settles, kind })
            .is_some()
        {
            anyhow::bail!("duplicate Task action declaration `{action}`");
        }
        Ok(())
    }
}

fn validate_option_payload<'a>(
    data: &Map<String, Value>,
    options: &'a HashMap<String, String>,
) -> anyhow::Result<&'a str> {
    reject_unknown_keys(data, &["choice", "label"])?;
    let choice = payload_string(data, "choice", true)?
        .ok_or_else(|| anyhow::anyhow!("Task action requires data.choice"))?;
    let label = options
        .get(choice)
        .ok_or_else(|| anyhow::anyhow!("unknown Task action choice: {choice}"))?;
    if let Some(provided) = payload_string(data, "label", false)?
        && provided != label
    {
        anyhow::bail!("Task action data.label does not match choice `{choice}`");
    }
    Ok(label)
}

fn validate_form_payload(
    data: &Map<String, Value>,
    fields: &HashMap<String, FieldSpec>,
) -> anyhow::Result<()> {
    if data.len() > MAX_FIELDS {
        anyhow::bail!("Task action data exceeds {MAX_FIELDS} fields");
    }
    for key in data.keys() {
        if !fields.contains_key(key) {
            anyhow::bail!("unknown Task action data field `{key}`");
        }
    }
    for (name, spec) in fields {
        let value = payload_string(data, name, spec.required)?;
        if spec.required && value.is_none_or(str::is_empty) {
            anyhow::bail!("Task action requires non-empty data.{name}");
        }
    }
    Ok(())
}

fn payload_string<'a>(
    data: &'a Map<String, Value>,
    key: &str,
    required: bool,
) -> anyhow::Result<Option<&'a str>> {
    let Some(value) = data.get(key) else {
        if required {
            anyhow::bail!("Task action requires data.{key}");
        }
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Task action data.{key} must be a string"))?;
    if value.len() > MAX_FIELD_STRING_BYTES {
        anyhow::bail!("Task action data.{key} exceeds {MAX_FIELD_STRING_BYTES} bytes");
    }
    Ok(Some(value))
}

fn reject_unknown_keys(data: &Map<String, Value>, allowed: &[&str]) -> anyhow::Result<()> {
    if let Some(key) = data.keys().find(|key| !allowed.contains(&key.as_str())) {
        anyhow::bail!("unknown Task action data field `{key}`");
    }
    Ok(())
}

fn validate_node(node: &Value, at: &str, depth: usize, nodes: &mut usize) -> anyhow::Result<()> {
    if depth > MAX_DEPTH {
        anyhow::bail!("Task UI exceeds maximum nesting depth at {at}");
    }
    *nodes += 1;
    if *nodes > MAX_NODES {
        anyhow::bail!("Task UI exceeds {MAX_NODES} components");
    }
    let object = node
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{at} must be a component object"))?;
    let kind = required_string(object, "type", at)?;
    match kind {
        "card" => {
            allowed(object, &["type", "children"], at)?;
            validate_children(object, at, depth, nodes, true)?;
        }
        "inset" => {
            allowed(object, &["type", "children"], at)?;
            validate_children(object, at, depth, nodes, true)?;
        }
        "eyebrow" => {
            allowed(object, &["type", "text", "tone", "boundary"], at)?;
            required_display_scalar(object, "text", at)?;
            optional_enum(object, "tone", &EYEBROW_TONES, at)?;
            optional_bool(object, "boundary", at)?;
        }
        "heading" => {
            allowed(object, &["type", "text", "level"], at)?;
            required_display_scalar(object, "text", at)?;
            optional_integer_range(object, "level", 1, 4, at)?;
        }
        "text" => {
            allowed(object, &["type", "text", "tone"], at)?;
            required_display_scalar(object, "text", at)?;
            optional_enum(object, "tone", &SEMANTIC_TONES, at)?;
        }
        "keyValue" => {
            allowed(object, &["type", "items"], at)?;
            validate_object_items(object, "items", at, |item, item_at| {
                allowed(item, &["label", "value", "tone"], item_at)?;
                required_string(item, "label", item_at)?;
                required_display_scalar(item, "value", item_at)?;
                optional_enum(item, "tone", &SEMANTIC_TONES, item_at)
            })?;
        }
        "badge" => {
            allowed(object, &["type", "label", "tone"], at)?;
            required_string(object, "label", at)?;
            optional_enum(object, "tone", &SEMANTIC_TONES, at)?;
        }
        "status" => {
            allowed(object, &["type", "state", "label", "tone"], at)?;
            required_string(object, "label", at)?;
            required_enum(object, "state", &STATUS_STATES, at)?;
            optional_enum(object, "tone", &SEMANTIC_TONES, at)?;
        }
        "divider" => allowed(object, &["type"], at)?,
        "optionList" => validate_option_list(object, at)?,
        "field" => {
            allowed(
                object,
                &[
                    "type",
                    "name",
                    "kind",
                    "label",
                    "placeholder",
                    "value",
                    "required",
                ],
                at,
            )?;
            required_string(object, "name", at)?;
            optional_enum(object, "kind", &["text"], at)?;
            optional_string(object, "label", at)?;
            optional_string(object, "placeholder", at)?;
            if let Some(value) = object.get("value")
                && !value.is_null()
                && !value.is_string()
            {
                anyhow::bail!("{at}.value must be null or a string");
            }
            optional_bool(object, "required", at)?;
        }
        "submit" => {
            allowed(
                object,
                &["type", "action", "label", "variant", "settles"],
                at,
            )?;
            optional_string(object, "action", at)?;
            required_string(object, "label", at)?;
            optional_enum(object, "variant", &["ghost"], at)?;
            optional_bool(object, "settles", at)?;
        }
        "viewSlot" => {
            allowed(
                object,
                &["type", "title", "variant", "lines", "rows", "view"],
                at,
            )?;
            optional_string(object, "title", at)?;
            optional_enum(object, "variant", &["diff", "table"], at)?;
            if let Some(view) = object.get("view") {
                crate::templates::validate(view)
                    .map_err(|error| anyhow::anyhow!("invalid {at}.view: {error}"))?;
            }
            for key in ["lines", "rows"] {
                if let Some(value) = object.get(key)
                    && !value.is_array()
                {
                    anyhow::bail!("{at}.{key} must be an array");
                }
            }
        }
        other => anyhow::bail!("unknown Task UI component `{other}` at {at}"),
    }
    Ok(())
}

const EYEBROW_TONES: [&str; 7] = [
    "default", "muted", "accent", "success", "warning", "danger", "neutral",
];

fn validate_option_list(object: &Map<String, Value>, at: &str) -> anyhow::Result<()> {
    allowed(object, &["type", "action", "settles", "options"], at)?;
    optional_string(object, "action", at)?;
    optional_bool(object, "settles", at)?;
    let mut keys = HashSet::new();
    let options = object
        .get("options")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("{at}.options must be an array"))?;
    if options.len() > MAX_CHOICES {
        anyhow::bail!("{at}.options exceeds {MAX_CHOICES} choices");
    }
    validate_object_items(object, "options", at, |option, option_at| {
        allowed(
            option,
            &["key", "label", "detail", "recommended"],
            option_at,
        )?;
        let key = required_string(option, "key", option_at)?;
        if !keys.insert(key.to_string()) {
            anyhow::bail!("duplicate option key `{key}` at {option_at}");
        }
        required_string(option, "label", option_at)?;
        optional_string(option, "detail", option_at)?;
        optional_bool(option, "recommended", option_at)
    })
}

fn validate_children(
    object: &Map<String, Value>,
    at: &str,
    depth: usize,
    nodes: &mut usize,
    required: bool,
) -> anyhow::Result<()> {
    let Some(children) = object.get("children") else {
        if required {
            anyhow::bail!("{at}.children is required");
        }
        return Ok(());
    };
    let children = children
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{at}.children must be an array"))?;
    for (index, child) in children.iter().enumerate() {
        validate_node(child, &format!("{at}.children[{index}]"), depth + 1, nodes)?;
    }
    Ok(())
}

fn validate_object_items<F>(
    object: &Map<String, Value>,
    key: &str,
    at: &str,
    mut validate_item: F,
) -> anyhow::Result<()>
where
    F: FnMut(&Map<String, Value>, &str) -> anyhow::Result<()>,
{
    let items = object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("{at}.{key} must be an array"))?;
    if items.is_empty() {
        anyhow::bail!("{at}.{key} must not be empty");
    }
    for (index, item) in items.iter().enumerate() {
        let item_at = format!("{at}.{key}[{index}]");
        let item = item
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("{item_at} must be an object"))?;
        validate_item(item, &item_at)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{MAX_ACTION_DATA_BYTES, action_contract, validate, validate_action};

    #[test]
    fn validates_adaptive_instrument_and_rejects_undeclared_power() {
        let ui = json!({
            "type": "card",
            "children": [
                { "type": "heading", "text": "Advance?" },
                {
                    "type": "optionList",
                    "action": "advance",
                    "settles": false,
                    "options": [{ "key": "A", "label": "Continue", "recommended": true }]
                }
            ]
        });
        validate(&ui).unwrap();
        assert_eq!(action_contract(&ui, "advance"), Some(false));
        assert_eq!(action_contract(&ui, "delete_everything"), None);
        assert!(validate(&json!({ "type": "script", "src": "no" })).is_err());
    }

    #[test]
    fn rejects_duplicate_actions_fields_and_unrendered_field_kinds() {
        let duplicate_action = json!({
            "type": "card",
            "children": [
                { "type": "submit", "action": "advance", "label": "One", "settles": false },
                { "type": "submit", "action": "advance", "label": "Two", "settles": false }
            ]
        });
        assert!(
            validate(&duplicate_action)
                .unwrap_err()
                .to_string()
                .contains("duplicate Task action")
        );
        let duplicate_field = json!({
            "type": "card",
            "children": [
                { "type": "field", "name": "answer", "kind": "text" },
                { "type": "field", "name": "answer", "kind": "text" },
                { "type": "submit", "action": "advance", "label": "Continue", "settles": false }
            ]
        });
        assert!(
            validate(&duplicate_field)
                .unwrap_err()
                .to_string()
                .contains("duplicate Task field")
        );
        assert!(validate(&json!({ "type": "field", "name": "x", "kind": "password" })).is_err());
    }

    #[test]
    fn form_action_payload_is_closed_typed_required_and_bounded() {
        let ui = json!({
            "type": "card",
            "children": [
                { "type": "field", "name": "confirmation", "kind": "text", "required": true },
                { "type": "field", "name": "note", "kind": "text" },
                { "type": "submit", "action": "advance", "label": "Continue", "settles": false }
            ]
        });
        let validated = validate_action(
            &ui,
            "advance",
            &json!({ "confirmation": "ready", "note": "bounded" }),
        )
        .unwrap();
        assert!(!validated.settles);
        assert_eq!(validated.choice_label, None);
        for hostile in [
            json!({}),
            json!({ "confirmation": "" }),
            json!({ "confirmation": 42 }),
            json!({ "confirmation": "ready", "unknown": true }),
        ] {
            assert!(validate_action(&ui, "advance", &hostile).is_err());
        }
        let oversized = "x".repeat(MAX_ACTION_DATA_BYTES + 1);
        assert!(validate_action(&ui, "advance", &json!({ "confirmation": oversized })).is_err());
        assert!(validate_action(&ui, "other-task-action", &json!({})).is_err());
    }

    #[test]
    fn option_action_accepts_only_a_declared_choice_and_matching_label() {
        let ui = json!({
            "type": "optionList",
            "action": "advance",
            "settles": false,
            "options": [
                { "key": "A", "label": "Continue" },
                { "key": "B", "label": "Pause" }
            ]
        });
        let validated = validate_action(
            &ui,
            "advance",
            &json!({ "choice": "A", "label": "Continue" }),
        )
        .unwrap();
        assert!(!validated.settles);
        assert_eq!(validated.choice_label.as_deref(), Some("Continue"));
        for hostile in [
            json!({}),
            json!({ "choice": "C" }),
            json!({ "choice": "A", "label": "Pause" }),
            json!({ "choice": "A", "extra": "injected" }),
        ] {
            assert!(validate_action(&ui, "advance", &hostile).is_err());
        }
    }

    #[test]
    fn bounds_choice_and_field_counts() {
        let choices = (0..17)
            .map(|index| json!({ "key": format!("{index}"), "label": format!("Choice {index}") }))
            .collect::<Vec<_>>();
        assert!(
            validate(&json!({ "type": "optionList", "options": choices }))
                .unwrap_err()
                .to_string()
                .contains("16 choices")
        );
        let fields = (0..17)
            .map(|index| json!({ "type": "field", "name": format!("field-{index}") }))
            .chain(std::iter::once(
                json!({ "type": "submit", "label": "Submit" }),
            ))
            .collect::<Vec<_>>();
        assert!(
            validate(&json!({ "type": "card", "children": fields }))
                .unwrap_err()
                .to_string()
                .contains("16 fields")
        );
    }
}
