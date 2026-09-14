use serde_json::{Map, Value};

pub(crate) const SEMANTIC_TONES: [&str; 5] = ["default", "muted", "success", "warning", "danger"];
pub(crate) const STATUS_STATES: [&str; 5] = ["neutral", "running", "success", "warning", "danger"];
pub(crate) const VIEW_FIELD_KINDS: [&str; 5] = ["text", "textarea", "number", "toggle", "select"];

pub(crate) fn allowed(object: &Map<String, Value>, keys: &[&str], at: &str) -> anyhow::Result<()> {
    if let Some(key) = object.keys().find(|key| !keys.contains(&key.as_str())) {
        anyhow::bail!("unknown property `{key}` at {at}");
    }
    Ok(())
}

pub(crate) fn required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    at: &str,
) -> anyhow::Result<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("{at}.{key} must be a non-empty string"))
}

pub(crate) fn optional_string(
    object: &Map<String, Value>,
    key: &str,
    at: &str,
) -> anyhow::Result<()> {
    if object.contains_key(key) {
        required_string(object, key, at)?;
    }
    Ok(())
}

pub(crate) fn required_bool(
    object: &Map<String, Value>,
    key: &str,
    at: &str,
) -> anyhow::Result<()> {
    if object.get(key).and_then(Value::as_bool).is_none() {
        anyhow::bail!("{at}.{key} must be a boolean");
    }
    Ok(())
}

pub(crate) fn optional_bool(
    object: &Map<String, Value>,
    key: &str,
    at: &str,
) -> anyhow::Result<()> {
    if object.get(key).is_some_and(|value| !value.is_boolean()) {
        anyhow::bail!("{at}.{key} must be a boolean");
    }
    Ok(())
}

pub(crate) fn required_display_scalar(
    object: &Map<String, Value>,
    key: &str,
    at: &str,
) -> anyhow::Result<()> {
    if !object.get(key).is_some_and(is_display_scalar) {
        anyhow::bail!("{at}.{key} must be a string, number, or boolean");
    }
    Ok(())
}

pub(crate) fn optional_display_scalar(
    object: &Map<String, Value>,
    key: &str,
    at: &str,
) -> anyhow::Result<()> {
    if let Some(value) = object.get(key)
        && !is_display_scalar(value)
    {
        anyhow::bail!("{at}.{key} must be a string, number, or boolean");
    }
    Ok(())
}

pub(crate) fn is_display_scalar(value: &Value) -> bool {
    value.is_string() || value.is_number() || value.is_boolean()
}

pub(crate) fn required_enum<'a>(
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

pub(crate) fn optional_enum(
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

pub(crate) fn required_number_range(
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

pub(crate) fn optional_integer_range(
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
