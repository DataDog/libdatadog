use std::collections::HashMap;
use std::hash::Hash;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct DebuggerPayload<S: Eq + Hash> {
    pub service: S,
    pub source: &'static str,
    pub timestamp: u64,
    pub debugger: DebuggerData<S>,
}

#[derive(Serialize, Deserialize)]
pub struct DebuggerData<S: Eq + Hash> {
    pub snapshot: Snapshot<S>,
}

#[derive(Serialize, Deserialize)]
pub struct Snapshot<S: Eq + Hash> {
    pub captures: Captures<S>,
    pub language: S,
    pub id: S,
    #[serde(rename = "exception-id")]
    pub exception_id: S,
    pub timestamp: u64,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Captures<S: Eq + Hash> {
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub lines: HashMap<u32, Capture<S>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<Capture<S>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#return: Option<Capture<S>>,
}

pub type Fields<S> = HashMap<S, Value<S>>;
#[derive(Default, Serialize, Deserialize)]
pub struct Capture<S: Eq + Hash> {
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    #[serde(rename = "staticFields")]
    pub static_fields: Fields<S>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub arguments: Fields<S>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub locals: Fields<S>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throwable: Option<Value<S>>,
}

#[derive(Serialize, Deserialize)]
pub struct Entry<S: Eq + Hash>(pub Value<S>, pub Value<S>);

#[derive(Default, Serialize, Deserialize)]
pub struct Value<S: Eq + Hash> {
    pub r#type: S,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<S>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub fields: Fields<S>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub elements: Vec<Value<S>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<Entry<S>>,
    #[serde(skip_serializing_if = "<&bool as std::ops::Not>::not")]
    #[serde(rename = "isNull")]
    pub is_null: bool,
    #[serde(skip_serializing_if = "<&bool as std::ops::Not>::not")]
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "notCapturedReason")]
    pub not_captured_reason: Option<S>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<S>,
}


