use crate::parse_json::ParseResult;
use json::JsonValue;

pub fn get<'a>(json: &'a JsonValue, name: &str) -> ParseResult<&'a JsonValue> {
    if json.has_key(name) {
        Ok(&json[name])
    } else {
        Err(())
    }
}
