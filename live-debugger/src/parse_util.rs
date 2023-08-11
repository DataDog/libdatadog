use json::JsonValue;

pub fn get<'a>(json: &'a JsonValue, name: &str) -> anyhow::Result<&'a JsonValue> {
    try_get(json, name).ok_or_else(|| anyhow::format_err!("Missing key {name}"))
}

pub fn try_get<'a>(json: &'a JsonValue, name: &str) -> Option<&'a JsonValue> {
    if json.has_key(name) {
        Some(&json[name])
    } else {
        None
    }
}
