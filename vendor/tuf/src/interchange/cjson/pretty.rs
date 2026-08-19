use serde::de::DeserializeOwned;
use serde::ser::Serialize;
use serde_json::{Map, Value};

use super::Json;
use crate::interchange::DataInterchange;
use crate::Result;

/// Pretty JSON data interchange.
///
/// This is identical to [Json] in all manners except for the `canonicalize` method. Instead of
/// writing the metadata in the canonical format, it first canonicalizes it, then pretty prints
/// the metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonPretty;

impl DataInterchange for JsonPretty {
    type RawData = serde_json::Value;

    /// ```
    /// # use tuf::interchange::{DataInterchange, JsonPretty};
    /// assert_eq!(JsonPretty::extension(), "json");
    /// ```
    fn extension() -> &'static str {
        Json::extension()
    }

    /// ```
    /// # use serde_json::json;
    /// # use tuf::interchange::{DataInterchange, JsonPretty};
    /// #
    /// let json = json!({
    ///     "o": {
    ///         "a": [1, 2, 3],
    ///         "s": "string",
    ///         "n": 123,
    ///         "t": true,
    ///         "f": false,
    ///         "0": null,
    ///     },
    /// });
    ///
    /// let bytes = JsonPretty::canonicalize(&json).unwrap();
    ///
    /// assert_eq!(&String::from_utf8(bytes).unwrap(), r#"{
    ///   "o": {
    ///     "0": null,
    ///     "a": [
    ///       1,
    ///       2,
    ///       3
    ///     ],
    ///     "f": false,
    ///     "n": 123,
    ///     "s": "string",
    ///     "t": true
    ///   }
    /// }"#);
    /// ```
    fn canonicalize(raw_data: &Self::RawData) -> Result<Vec<u8>> {
        // Sort explicitly: `Value::Object` is `IndexMap` (insertion order) when any workspace
        // crate enables `serde_json/preserve_order`, so we can't rely on the `Map` type alone.
        Ok(serde_json::to_vec_pretty(&with_sorted_keys(raw_data))?)
    }

    /// ```
    /// # use serde_derive::Deserialize;
    /// # use serde_json::json;
    /// # use std::collections::HashMap;
    /// # use tuf::interchange::{DataInterchange, JsonPretty};
    /// #
    /// #[derive(Deserialize, Debug, PartialEq)]
    /// struct Thing {
    ///    foo: String,
    ///    bar: String,
    /// }
    ///
    /// let jsn = json!({"foo": "wat", "bar": "lol"});
    /// let thing = Thing { foo: "wat".into(), bar: "lol".into() };
    /// let de: Thing = JsonPretty::deserialize(&jsn).unwrap();
    /// assert_eq!(de, thing);
    /// ```
    fn deserialize<T>(raw_data: &Self::RawData) -> Result<T>
    where
        T: DeserializeOwned,
    {
        Json::deserialize(raw_data)
    }

    /// ```
    /// # use serde_derive::Serialize;
    /// # use serde_json::json;
    /// # use std::collections::HashMap;
    /// # use tuf::interchange::{DataInterchange, JsonPretty};
    /// #
    /// #[derive(Serialize)]
    /// struct Thing {
    ///    foo: String,
    ///    bar: String,
    /// }
    ///
    /// let jsn = json!({"foo": "wat", "bar": "lol"});
    /// let thing = Thing { foo: "wat".into(), bar: "lol".into() };
    /// let se: serde_json::Value = JsonPretty::serialize(&thing).unwrap();
    /// assert_eq!(se, jsn);
    /// ```
    fn serialize<T>(data: &T) -> Result<Self::RawData>
    where
        T: Serialize,
    {
        Json::serialize(data)
    }

    /// ```
    /// # use tuf::interchange::{DataInterchange, JsonPretty};
    /// # use std::collections::HashMap;
    /// let jsn: &[u8] = br#"{"foo": "bar", "baz": "quux"}"#;
    /// let _: HashMap<String, String> = JsonPretty::from_slice(&jsn).unwrap();
    /// ```
    fn from_slice<T>(slice: &[u8]) -> Result<T>
    where
        T: DeserializeOwned,
    {
        Json::from_slice(slice)
    }
}

/// Rebuild a `Value` with every object's keys inserted in sorted order, so re-serialization
/// emits them sorted regardless of whether `serde_json::Map` is `BTreeMap` or `IndexMap`.
fn with_sorted_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut sorted = Map::with_capacity(entries.len());
            for (k, v) in entries {
                sorted.insert(k.clone(), with_sorted_keys(v));
            }
            Value::Object(sorted)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(with_sorted_keys).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Strings with control characters (literal newlines from PEM keyvals, tabs, etc.) must
    /// canonicalize without error.
    #[test]
    fn canonicalize_handles_strings_with_control_characters() {
        let value = json!({
            "key_with_newlines": "-----BEGIN PUBLIC KEY-----\nABC\n-----END PUBLIC KEY-----\n",
            "key_with_tab": "a\tb",
        });
        let bytes = JsonPretty::canonicalize(&value).expect("must not fail on control chars");

        // Round-trips back to the same value.
        let reparsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(reparsed, value);

        // Pretty output sorts top-level keys (alphabetical: `key_with_newlines`, `key_with_tab`).
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.find("key_with_newlines").unwrap() < s.find("key_with_tab").unwrap());
    }

    /// Object keys must come out sorted even when the underlying `Map` preserves insertion
    /// order (`tuf/Cargo.toml` enables `serde_json/preserve_order` as a dev-dep so this runs
    /// against `IndexMap`). Inserting in reverse-alphabetical order would otherwise yield
    /// reverse-alphabetical output if the canonicalizer didn't explicitly sort.
    #[test]
    fn canonicalize_sorts_keys_recursively_against_insertion_order() {
        let mut top = serde_json::Map::new();
        // Insert in reverse-alphabetical order; under preserve_order this is the iteration
        // order, under BTreeMap it gets sorted.
        let mut nested = serde_json::Map::new();
        nested.insert("z_inner".to_string(), json!(1));
        nested.insert("a_inner".to_string(), json!(2));
        top.insert("z_top".to_string(), serde_json::Value::Object(nested));
        top.insert("a_top".to_string(), json!("first alphabetically"));

        let value = serde_json::Value::Object(top);
        let bytes = JsonPretty::canonicalize(&value).unwrap();
        let pretty = std::str::from_utf8(&bytes).unwrap();

        // Top-level: a_top before z_top.
        let a_top = pretty.find("a_top").unwrap();
        let z_top = pretty.find("z_top").unwrap();
        assert!(a_top < z_top, "top-level keys must sort: {pretty}");

        // Nested object: a_inner before z_inner.
        let a_inner = pretty.find("a_inner").unwrap();
        let z_inner = pretty.find("z_inner").unwrap();
        assert!(a_inner < z_inner, "nested keys must sort: {pretty}");
    }
}
