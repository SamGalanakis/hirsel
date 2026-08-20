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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn strings_are_trimmed_and_optional_strings_are_non_empty_when_present() {
        let valid = json!({ "required": " value ", "optional": "value" });
        let valid = valid.as_object().unwrap();
        assert_eq!(
            required_string(valid, "required", "node").unwrap(),
            " value "
        );
        optional_string(valid, "optional", "node").unwrap();
        optional_string(valid, "missing", "node").unwrap();

        for invalid in [json!(""), json!(" \t\n"), json!(42)] {
            let object = json!({ "value": invalid });
            let object = object.as_object().unwrap();
            assert!(required_string(object, "value", "node").is_err());
            assert!(optional_string(object, "value", "node").is_err());
        }
    }

    #[test]
    fn object_boolean_scalar_and_enum_primitives_are_closed() {
        let object = json!({
            "enabled": true,
            "optionalEnabled": false,
            "label": 42,
            "optionalLabel": "ready",
            "state": "running"
        });
        let object = object.as_object().unwrap();
        allowed(
            object,
            &[
                "enabled",
                "optionalEnabled",
                "label",
                "optionalLabel",
                "state",
            ],
            "node",
        )
        .unwrap();
        required_bool(object, "enabled", "node").unwrap();
        optional_bool(object, "optionalEnabled", "node").unwrap();
        required_display_scalar(object, "label", "node").unwrap();
        optional_display_scalar(object, "optionalLabel", "node").unwrap();
        assert_eq!(
            required_enum(object, "state", &STATUS_STATES, "node").unwrap(),
            "running"
        );
        optional_enum(object, "state", &STATUS_STATES, "node").unwrap();

        assert!(allowed(object, &["enabled"], "node").is_err());
        assert!(required_bool(object, "missing", "node").is_err());
        assert!(optional_bool(object, "label", "node").is_err());
        assert!(required_display_scalar(object, "missing", "node").is_err());
        assert!(optional_display_scalar(object, "enabled", "node").is_ok());
        assert!(required_enum(object, "state", &["done"], "node").is_err());
    }

    #[test]
    fn numeric_ranges_enforce_type_and_bounds() {
        let object = json!({ "ratio": 0.5, "level": 3 });
        let object = object.as_object().unwrap();
        required_number_range(object, "ratio", 0.0, 1.0, "node").unwrap();
        optional_integer_range(object, "level", 1, 4, "node").unwrap();
        optional_integer_range(object, "missing", 1, 4, "node").unwrap();

        assert!(required_number_range(object, "ratio", 0.6, 1.0, "node").is_err());
        assert!(optional_integer_range(object, "level", 4, 8, "node").is_err());
        assert!(optional_integer_range(object, "ratio", 0, 1, "node").is_err());
    }
}
