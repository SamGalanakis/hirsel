use std::collections::BTreeSet;

use serde_json::{Map, Value};

pub fn validate(spec: &Value) -> anyhow::Result<()> {
    validate_node(spec, "spec")
}

fn validate_node(node: &Value, at: &str) -> anyhow::Result<()> {
    let object = node
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{at} must be a component object"))?;
    let component = required_string(object, "type", at)?;
    match component {
        "card" => {
            allowed(object, &["type", "title", "subtitle", "children"], at)?;
            optional_string(object, "title", at)?;
            optional_string(object, "subtitle", at)?;
            children(object, "children", true, at)?;
        }
        "stack" => {
            allowed(object, &["type", "gap", "children"], at)?;
            optional_enum(object, "gap", &["xs", "sm", "md", "lg"], at)?;
            children(object, "children", true, at)?;
        }
        "row" => {
            allowed(object, &["type", "gap", "align", "wrap", "children"], at)?;
            optional_enum(object, "gap", &["xs", "sm", "md", "lg"], at)?;
            optional_enum(object, "align", &["start", "center", "end", "stretch"], at)?;
            optional_bool(object, "wrap", at)?;
            children(object, "children", true, at)?;
        }
        "heading" => {
            allowed(object, &["type", "text", "level"], at)?;
            required_string(object, "text", at)?;
            optional_integer_range(object, "level", 1, 4, at)?;
        }
        "text" => {
            allowed(object, &["type", "text", "tone"], at)?;
            required_display_scalar(object, "text", at)?;
            optional_tone(object, "tone", at)?;
        }
        "keyValue" => {
            allowed(object, &["type", "items"], at)?;
            object_array(object, "items", at, |item, item_at| {
                allowed(item, &["label", "value", "tone"], item_at)?;
                required_string(item, "label", item_at)?;
                required_display_scalar(item, "value", item_at)?;
                optional_tone(item, "tone", item_at)
            })?;
        }
        "table" => validate_table(object, at)?,
        "list" => {
            allowed(object, &["type", "items", "ordered"], at)?;
            optional_bool(object, "ordered", at)?;
            object_array(object, "items", at, |item, item_at| {
                allowed(item, &["text", "tone"], item_at)?;
                required_display_scalar(item, "text", item_at)?;
                optional_tone(item, "tone", item_at)
            })?;
        }
        "checklist" => {
            allowed(object, &["type", "items"], at)?;
            object_array(object, "items", at, |item, item_at| {
                allowed(item, &["label", "checked", "detail"], item_at)?;
                required_string(item, "label", item_at)?;
                required_bool(item, "checked", item_at)?;
                optional_string(item, "detail", item_at)
            })?;
        }
        "badge" => {
            allowed(object, &["type", "label", "tone"], at)?;
            required_string(object, "label", at)?;
            optional_tone(object, "tone", at)?;
        }
        "status" => {
            allowed(object, &["type", "label", "state"], at)?;
            required_string(object, "label", at)?;
            required_enum(
                object,
                "state",
                &["neutral", "running", "success", "warning", "danger"],
                at,
            )?;
        }
        "progress" => {
            allowed(object, &["type", "value", "label"], at)?;
            required_number_range(object, "value", 0.0, 1.0, at)?;
            optional_string(object, "label", at)?;
        }
        "callout" => {
            allowed(object, &["type", "tone", "title", "body"], at)?;
            optional_enum(
                object,
                "tone",
                &["default", "success", "warning", "danger"],
                at,
            )?;
            optional_string(object, "title", at)?;
            required_string(object, "body", at)?;
        }
        "divider" => allowed(object, &["type"], at)?,
        "action" => {
            allowed(object, &["type", "label", "action", "data", "variant"], at)?;
            required_string(object, "label", at)?;
            required_string(object, "action", at)?;
            optional_enum(object, "variant", &["primary", "secondary", "danger"], at)?;
        }
        "optionSet" => validate_option_set(object, at)?,
        "field" => validate_field(object, at)?,
        "form" => validate_form(object, at)?,
        other => anyhow::bail!("unknown component type `{other}` at {at}"),
    }
    Ok(())
}

fn validate_table(object: &Map<String, Value>, at: &str) -> anyhow::Result<()> {
    allowed(object, &["type", "columns", "rows", "caption"], at)?;
    optional_string(object, "caption", at)?;
    let mut keys = BTreeSet::new();
    object_array(object, "columns", at, |column, column_at| {
        allowed(column, &["key", "label", "align"], column_at)?;
        let key = required_string(column, "key", column_at)?;
        if !keys.insert(key.to_string()) {
            anyhow::bail!("duplicate table column key `{key}` at {column_at}");
        }
        required_string(column, "label", column_at)?;
        optional_enum(column, "align", &["start", "center", "end"], column_at)
    })?;
    let rows = object
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("{at}.rows must be an array"))?;
    for (index, row) in rows.iter().enumerate() {
        let row_at = format!("{at}.rows[{index}]");
        let row = row
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("{row_at} must be an object"))?;
        for key in &keys {
            let value = row
                .get(key)
                .ok_or_else(|| anyhow::anyhow!("{row_at} is missing column `{key}`"))?;
            if !is_display_scalar(value) {
                anyhow::bail!("{row_at}.{key} must be a string, number, or boolean");
            }
        }
        if let Some(extra) = row.keys().find(|key| !keys.contains(*key)) {
            anyhow::bail!("unknown table column `{extra}` at {row_at}");
        }
    }
    Ok(())
}

fn validate_option_set(object: &Map<String, Value>, at: &str) -> anyhow::Result<()> {
    allowed(
        object,
        &["type", "action", "label", "choices", "selected"],
        at,
    )?;
    required_string(object, "action", at)?;
    optional_string(object, "label", at)?;
    optional_display_scalar(object, "selected", at)?;
    object_array(object, "choices", at, |choice, choice_at| {
        allowed(choice, &["label", "value", "description"], choice_at)?;
        required_string(choice, "label", choice_at)?;
        required_display_scalar(choice, "value", choice_at)?;
        optional_string(choice, "description", choice_at)
    })
}

fn validate_field(object: &Map<String, Value>, at: &str) -> anyhow::Result<()> {
    allowed(
        object,
        &[
            "type",
            "name",
            "label",
            "kind",
            "value",
            "placeholder",
            "required",
            "options",
        ],
        at,
    )?;
    required_string(object, "name", at)?;
    required_string(object, "label", at)?;
    let kind = required_enum(
        object,
        "kind",
        &["text", "textarea", "number", "toggle", "select"],
        at,
    )?;
    optional_string(object, "placeholder", at)?;
    optional_bool(object, "required", at)?;
    if let Some(value) = object.get("value")
        && !is_display_scalar(value)
        && !value.is_null()
    {
        anyhow::bail!("{at}.value must be null, a string, number, or boolean");
    }
    match (kind, object.get("options")) {
        ("select", Some(options)) => validate_field_options(options, at),
        ("select", None) => anyhow::bail!("{at}.options is required for a select field"),
        (_, Some(_)) => anyhow::bail!("{at}.options is only valid for a select field"),
        (_, None) => Ok(()),
    }
}

fn validate_field_options(options: &Value, at: &str) -> anyhow::Result<()> {
    let options = options
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{at}.options must be an array"))?;
    for (index, option) in options.iter().enumerate() {
        let option_at = format!("{at}.options[{index}]");
        let option = option
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("{option_at} must be an object"))?;
        allowed(option, &["label", "value"], &option_at)?;
        required_string(option, "label", &option_at)?;
        required_display_scalar(option, "value", &option_at)?;
    }
    Ok(())
}

fn validate_form(object: &Map<String, Value>, at: &str) -> anyhow::Result<()> {
    allowed(object, &["type", "action", "fields", "submitLabel"], at)?;
    required_string(object, "action", at)?;
    optional_string(object, "submitLabel", at)?;
    let fields = object
        .get("fields")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("{at}.fields must be an array"))?;
    for (index, field) in fields.iter().enumerate() {
        let field_at = format!("{at}.fields[{index}]");
        validate_node(field, &field_at)?;
        if field.get("type").and_then(Value::as_str) != Some("field") {
            anyhow::bail!("{field_at} must be a field component");
        }
    }
    Ok(())
}

fn allowed(object: &Map<String, Value>, keys: &[&str], at: &str) -> anyhow::Result<()> {
    if let Some(key) = object.keys().find(|key| !keys.contains(&key.as_str())) {
        anyhow::bail!("unknown property `{key}` at {at}");
    }
    Ok(())
}

fn children(
    object: &Map<String, Value>,
    key: &str,
    required: bool,
    at: &str,
) -> anyhow::Result<()> {
    let Some(value) = object.get(key) else {
        if required {
            anyhow::bail!("missing required array `{key}` at {at}");
        }
        return Ok(());
    };
    let values = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{at}.{key} must be an array"))?;
    for (index, child) in values.iter().enumerate() {
        validate_node(child, &format!("{at}.{key}[{index}]"))?;
    }
    Ok(())
}

fn object_array<F>(
    object: &Map<String, Value>,
    key: &str,
    at: &str,
    mut validate_item: F,
) -> anyhow::Result<()>
where
    F: FnMut(&Map<String, Value>, &str) -> anyhow::Result<()>,
{
    let values = object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("{at}.{key} must be an array"))?;
    for (index, item) in values.iter().enumerate() {
        let item_at = format!("{at}.{key}[{index}]");
        let item = item
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("{item_at} must be an object"))?;
        validate_item(item, &item_at)?;
    }
    Ok(())
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    at: &str,
) -> anyhow::Result<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{at}.{key} must be a non-empty string"))
}

fn optional_string(object: &Map<String, Value>, key: &str, at: &str) -> anyhow::Result<()> {
    if let Some(value) = object.get(key)
        && !value.is_string()
    {
        anyhow::bail!("{at}.{key} must be a string");
    }
    Ok(())
}

fn required_bool(object: &Map<String, Value>, key: &str, at: &str) -> anyhow::Result<()> {
    if object.get(key).and_then(Value::as_bool).is_none() {
        anyhow::bail!("{at}.{key} must be a boolean");
    }
    Ok(())
}

fn optional_bool(object: &Map<String, Value>, key: &str, at: &str) -> anyhow::Result<()> {
    if let Some(value) = object.get(key)
        && !value.is_boolean()
    {
        anyhow::bail!("{at}.{key} must be a boolean");
    }
    Ok(())
}

fn required_display_scalar(object: &Map<String, Value>, key: &str, at: &str) -> anyhow::Result<()> {
    if !object.get(key).is_some_and(is_display_scalar) {
        anyhow::bail!("{at}.{key} must be a string, number, or boolean");
    }
    Ok(())
}

fn optional_display_scalar(object: &Map<String, Value>, key: &str, at: &str) -> anyhow::Result<()> {
    if let Some(value) = object.get(key)
        && !is_display_scalar(value)
    {
        anyhow::bail!("{at}.{key} must be a string, number, or boolean");
    }
    Ok(())
}

fn is_display_scalar(value: &Value) -> bool {
    value.is_string() || value.is_number() || value.is_boolean()
}

fn optional_tone(object: &Map<String, Value>, key: &str, at: &str) -> anyhow::Result<()> {
    optional_enum(
        object,
        key,
        &["default", "muted", "success", "warning", "danger"],
        at,
    )
}

fn required_enum<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    choices: &[&str],
    at: &str,
) -> anyhow::Result<&'a str> {
    let value = required_string(object, key, at)?;
    if !choices.contains(&value) {
        anyhow::bail!("{at}.{key} must be one of: {}", choices.join(", "));
    }
    Ok(value)
}

fn optional_enum(
    object: &Map<String, Value>,
    key: &str,
    choices: &[&str],
    at: &str,
) -> anyhow::Result<()> {
    if object.contains_key(key) {
        required_enum(object, key, choices, at)?;
    }
    Ok(())
}

fn required_number_range(
    object: &Map<String, Value>,
    key: &str,
    min: f64,
    max: f64,
    at: &str,
) -> anyhow::Result<()> {
    let value = object
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow::anyhow!("{at}.{key} must be a number"))?;
    if !(min..=max).contains(&value) {
        anyhow::bail!("{at}.{key} must be between {min} and {max}");
    }
    Ok(())
}

fn optional_integer_range(
    object: &Map<String, Value>,
    key: &str,
    min: u64,
    max: u64,
    at: &str,
) -> anyhow::Result<()> {
    if let Some(raw) = object.get(key) {
        let value = raw
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("{at}.{key} must be an integer"))?;
        if !(min..=max).contains(&value) {
            anyhow::bail!("{at}.{key} must be between {min} and {max}");
        }
    }
    Ok(())
}
