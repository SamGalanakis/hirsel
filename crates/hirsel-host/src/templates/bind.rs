use serde_json::{Map, Value};

pub fn bind_spec(spec: &Value, params: &Value) -> anyhow::Result<Value> {
    if !params.is_object() {
        anyhow::bail!("view params must be an object");
    }
    bind_value(spec, params, params, "spec")
}

fn bind_value(value: &Value, root: &Value, current: &Value, at: &str) -> anyhow::Result<Value> {
    match value {
        Value::String(text) => bind_string(text, root, current, at),
        Value::Array(values) => {
            let mut resolved = Vec::new();
            for (index, item) in values.iter().enumerate() {
                let item_path = format!("{at}[{index}]");
                if let Some((path, template)) = each_directive(item)? {
                    let items = lookup_binding(root, current, path).ok_or_else(|| {
                        anyhow::anyhow!(
                            "missing each binding `{{{{#each {path}}}}}` at {item_path}"
                        )
                    })?;
                    let items = items.as_array().ok_or_else(|| {
                        anyhow::anyhow!("each binding `{path}` at {item_path} must be an array")
                    })?;
                    for (each_index, each_item) in items.iter().enumerate() {
                        resolved.push(bind_value(
                            template,
                            root,
                            each_item,
                            &format!("{item_path}{{{each_index}}}"),
                        )?);
                    }
                } else {
                    resolved.push(bind_value(item, root, current, &item_path)?);
                }
            }
            Ok(Value::Array(resolved))
        }
        Value::Object(object) => {
            let mut resolved = Map::new();
            for (key, child) in object {
                resolved.insert(
                    key.clone(),
                    bind_value(child, root, current, &format!("{at}.{key}"))?,
                );
            }
            Ok(Value::Object(resolved))
        }
        _ => Ok(value.clone()),
    }
}

fn each_directive(value: &Value) -> anyhow::Result<Option<(&str, &Value)>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if object.len() != 1 {
        return Ok(None);
    }
    let (key, template) = object.iter().next().expect("one entry");
    let Some(inner) = key
        .strip_prefix("{{#each ")
        .and_then(|value| value.strip_suffix("}}"))
    else {
        return Ok(None);
    };
    let path = inner.trim();
    if path.is_empty() {
        anyhow::bail!("each binding path must not be empty");
    }
    Ok(Some((path, template)))
}

fn bind_string(text: &str, root: &Value, current: &Value, at: &str) -> anyhow::Result<Value> {
    if let Some(path) = text
        .strip_prefix("{{")
        .and_then(|value| value.strip_suffix("}}"))
        .filter(|value| !value.contains("{{") && !value.starts_with("#each "))
    {
        let path = path.trim();
        return lookup_binding(root, current, path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing binding `{{{{{path}}}}}` at {at}"));
    }

    let mut output = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        output.push_str(&rest[..start]);
        let binding = &rest[start + 2..];
        let end = binding
            .find("}}")
            .ok_or_else(|| anyhow::anyhow!("unterminated binding at {at}"))?;
        let path = binding[..end].trim();
        if path.starts_with("#each ") {
            anyhow::bail!("each bindings are only valid as array entries at {at}");
        }
        let value = lookup_binding(root, current, path)
            .ok_or_else(|| anyhow::anyhow!("missing binding `{{{{{path}}}}}` at {at}"))?;
        match value {
            Value::Null => {}
            Value::String(value) => output.push_str(value),
            other => output.push_str(&serde_json::to_string(other)?),
        }
        rest = &binding[end + 2..];
    }
    output.push_str(rest);
    Ok(Value::String(output))
}

fn lookup_binding<'a>(root: &'a Value, current: &'a Value, path: &str) -> Option<&'a Value> {
    if path == "this" {
        return Some(current);
    }
    if let Some(path) = path.strip_prefix("this.") {
        return descend(current, path);
    }
    if path == "@root" {
        return Some(root);
    }
    if let Some(path) = path.strip_prefix("@root.") {
        return descend(root, path);
    }
    descend(current, path).or_else(|| descend(root, path))
}

fn descend<'a>(mut value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(value);
    }
    for segment in path.split('.') {
        value = match value {
            Value::Object(object) => object.get(segment)?,
            Value::Array(array) => array.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(value)
}
