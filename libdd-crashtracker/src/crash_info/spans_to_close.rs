// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Spans a crash ends, described by an SDK for the receiver to finish and send
//! to the agent. See [`SpansToClose`] for the wire format, and RFC 0016.

use core::fmt;
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};

/// The open spans of one process, which the crash ends.
///
/// # Wire format
///
/// One line of JSON between the markers; a report may carry several blocks:
///
/// ```text
/// DD_CRASHTRACK_BEGIN_SPANS_TO_CLOSE
/// {"version":1,"agent_url":"http://agent:8126","process":{...},"traces":[...]}
/// DD_CRASHTRACK_END_SPANS_TO_CLOSE
/// ```
///
/// The SDK describes spans, not an agent request: the receiver picks the
/// trace format, writes every HTTP header, and decides what ending a span at
/// a crash means (see `receiver::spans_to_close`). `agent_url` is optional:
/// without it the spans go to the agent the crash report itself is sent to.
///
/// - IDs are lowercase hex strings: 32 digits for a trace ID, 16 for a span ID.
/// - Attributes are flat (`"ml.batch_size"`, not nested objects). A value is a string, a boolean,
///   an integer (a JSON number without `.`, `e` or `E` that fits in `i64`) or a float (any other
///   number). Arrays, objects and `null` are rejected.
/// - Unknown fields are ignored, so SDKs can add optional ones. A change to the meaning of a field
///   bumps `version`, which receivers reject when they do not know it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpansToClose {
    /// Base agent URL, `http://` or `unix://`. `None` sends the spans to the
    /// crash tracker's own agent endpoint.
    pub agent_url: Option<String>,
    pub process: ProcessToClose,
    pub traces: Vec<TraceToClose>,
}

/// Process-level fields (a v1 tracer payload's). Empty means unset.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ProcessToClose {
    pub language: String,
    pub language_version: String,
    pub tracer_version: String,
    pub runtime_id: String,
    pub container_id: String,
    pub hostname: String,
    pub env: String,
    pub app_version: String,
    pub attributes: Attributes,
}

/// One trace (a v1 trace chunk).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TraceToClose {
    #[serde(deserialize_with = "hex_u128")]
    pub trace_id: u128,
    /// The sampling priority; the receiver keeps traces without one.
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub sampling_mechanism: Option<u32>,
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub attributes: Attributes,
    pub spans: Vec<SpanToClose>,
}

/// One open span. Its end and error are the receiver's to set.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SpanToClose {
    #[serde(deserialize_with = "hex_u64")]
    pub span_id: u64,
    /// Zero for a root span.
    #[serde(default, deserialize_with = "hex_u64")]
    pub parent_id: u64,
    pub service: String,
    pub name: String,
    /// The name when absent.
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub r#type: String,
    pub start_unix_ns: i64,
    /// `internal`, `server`, `client`, `producer` or `consumer`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub env: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub component: String,
    #[serde(default)]
    pub attributes: Attributes,
}

/// An attribute value an SDK can stage.
#[derive(Debug, Clone, PartialEq)]
pub enum ScalarValue {
    String(String),
    Bool(bool),
    Int(i64),
    Float(f64),
}

/// Attributes in the order the SDK wrote them; a repeated key's last value wins.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Attributes(pub Vec<(String, ScalarValue)>);

#[derive(Deserialize)]
struct Version {
    version: u32,
}

#[derive(Deserialize)]
struct BlockV1 {
    #[serde(default)]
    agent_url: Option<String>,
    #[serde(default)]
    process: ProcessToClose,
    traces: Vec<TraceToClose>,
}

impl SpansToClose {
    /// The schema version this receiver reads.
    pub const VERSION: u32 = 1;
    /// The longest block line a receiver accepts.
    pub const MAX_LEN: usize = 64 * 1024;

    /// Parses a block's JSON line.
    pub fn parse(line: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            line.len() <= Self::MAX_LEN,
            "{} bytes, over the {}-byte limit",
            line.len(),
            Self::MAX_LEN
        );
        let Version { version } = serde_json::from_str(line)?;
        anyhow::ensure!(version == Self::VERSION, "unsupported version {version}");
        let block: BlockV1 = serde_json::from_str(line)?;
        anyhow::ensure!(block.traces.iter().any(|t| !t.spans.is_empty()), "no spans");
        Ok(Self {
            // An empty URL says no more than an absent one.
            agent_url: block.agent_url.filter(|url| !url.is_empty()),
            process: block.process,
            traces: block.traces,
        })
    }
}

fn hex_digits<'de, D: Deserializer<'de>>(d: D, digits: usize) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    if s.len() != digits || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(de::Error::custom(format!(
            "expected {digits} hex digits, got {s:?}"
        )));
    }
    Ok(s)
}

fn hex_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let s = hex_digits(d, 16)?;
    u64::from_str_radix(&s, 16).map_err(de::Error::custom)
}

fn hex_u128<'de, D: Deserializer<'de>>(d: D) -> Result<u128, D::Error> {
    let s = hex_digits(d, 32)?;
    u128::from_str_radix(&s, 16).map_err(de::Error::custom)
}

impl<'de> Deserialize<'de> for ScalarValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct ScalarVisitor;
        impl Visitor<'_> for ScalarVisitor {
            type Value = ScalarValue;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string, boolean or number")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<ScalarValue, E> {
                Ok(ScalarValue::Bool(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<ScalarValue, E> {
                Ok(ScalarValue::Int(v))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<ScalarValue, E> {
                // An integer past i64 keeps its magnitude as a float.
                #[allow(clippy::cast_precision_loss)]
                Ok(i64::try_from(v).map_or(ScalarValue::Float(v as f64), ScalarValue::Int))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<ScalarValue, E> {
                Ok(ScalarValue::Float(v))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<ScalarValue, E> {
                Ok(ScalarValue::String(v.to_string()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<ScalarValue, E> {
                Ok(ScalarValue::String(v))
            }
        }
        d.deserialize_any(ScalarVisitor)
    }
}

impl<'de> Deserialize<'de> for Attributes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct AttributesVisitor;
        impl<'de> Visitor<'de> for AttributesVisitor {
            type Value = Attributes;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object of attributes")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Attributes, A::Error> {
                let mut attributes = Vec::with_capacity(map.size_hint().unwrap_or(0));
                while let Some(entry) = map.next_entry::<String, ScalarValue>()? {
                    attributes.push(entry);
                }
                Ok(Attributes(attributes))
            }
        }
        d.deserialize_map(AttributesVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: &str = r#"{"version":1,"agent_url":"unix:///var/run/datadog/apm.socket",
        "process":{"language":"python","runtime_id":"r-1","attributes":{"_dd.tags.process":"a:b"}},
        "traces":[{"trace_id":"0000000000000000a3ce929d0e0e4736","priority":1,"origin":"lambda",
          "spans":[{"span_id":"ffffffffffffffff","parent_id":"0000000000000007","service":"trainer",
            "name":"training.rank","type":"custom","start_unix_ns":1700000000000000000,
            "kind":"internal","env":"dev",
            "attributes":{"rank":1,"ml.batch_size":16,"loss":0.5,"big":18446744073709551615,
              "ok":true,"ml.dataset":"synthetic","whole":16.0}}]}]}"#;

    #[test]
    fn parses_every_field() {
        let block = SpansToClose::parse(BLOCK).unwrap();
        assert_eq!(
            block.agent_url.as_deref(),
            Some("unix:///var/run/datadog/apm.socket")
        );
        assert_eq!(block.process.language, "python");
        assert_eq!(block.process.runtime_id, "r-1");
        assert_eq!(
            block.process.attributes.0,
            vec![(
                "_dd.tags.process".to_string(),
                ScalarValue::String("a:b".to_string())
            )]
        );
        let trace = &block.traces[0];
        assert_eq!(trace.trace_id, 0xa3ce929d0e0e4736);
        assert_eq!(trace.priority, Some(1));
        assert_eq!(trace.sampling_mechanism, None);
        assert_eq!(trace.origin, "lambda");
        let span = &trace.spans[0];
        assert_eq!(span.span_id, u64::MAX);
        assert_eq!(span.parent_id, 7);
        assert_eq!(span.resource, None);
        assert_eq!(span.r#type, "custom");
        assert_eq!(span.kind.as_deref(), Some("internal"));
        assert_eq!(span.env, "dev");
        assert_eq!(
            span.attributes.0,
            vec![
                ("rank".to_string(), ScalarValue::Int(1)),
                ("ml.batch_size".to_string(), ScalarValue::Int(16)),
                ("loss".to_string(), ScalarValue::Float(0.5)),
                (
                    "big".to_string(),
                    ScalarValue::Float(18446744073709551615.0)
                ),
                ("ok".to_string(), ScalarValue::Bool(true)),
                (
                    "ml.dataset".to_string(),
                    ScalarValue::String("synthetic".to_string())
                ),
                ("whole".to_string(), ScalarValue::Float(16.0)),
            ]
        );
    }

    #[test]
    fn optional_fields_default_and_unknown_fields_are_ignored() {
        let block = SpansToClose::parse(
            r#"{"version":1,"agent_url":"http://a","future":{"x":[1]},"traces":[{
                "trace_id":"00000000000000000000000000000001","later":1,
                "spans":[{"span_id":"0000000000000002","service":"s","name":"n",
                  "start_unix_ns":5,"later":[1,2]}]}]}"#,
        )
        .unwrap();
        assert_eq!(block.process, ProcessToClose::default());
        let span = &block.traces[0].spans[0];
        assert_eq!(span.parent_id, 0);
        assert!(span.attributes.0.is_empty());
    }

    #[test]
    fn agent_url_is_optional() {
        let span = r#""span_id":"0000000000000002","service":"s","name":"n","start_unix_ns":5"#;
        let trace = format!(
            r#""traces":[{{"trace_id":"00000000000000000000000000000001","spans":[{{{span}}}]}}]"#
        );
        for block in [
            format!(r#"{{"version":1,{trace}}}"#),
            format!(r#"{{"version":1,"agent_url":"",{trace}}}"#),
        ] {
            assert_eq!(
                SpansToClose::parse(&block).unwrap().agent_url,
                None,
                "{block}"
            );
        }
    }

    #[test]
    fn rejects_what_it_cannot_read_faithfully() {
        let span = r#""span_id":"0000000000000002","service":"s","name":"n","start_unix_ns":5"#;
        let trace_id = r#""trace_id":"00000000000000000000000000000001""#;
        for (block, why) in [
            (
                r#"{"version":2,"agent_url":"a","traces":[]}"#.to_string(),
                "version",
            ),
            (
                r#"{"version":1,"agent_url":"a","traces":[]}"#.to_string(),
                "spans",
            ),
            (
                format!(
                    r#"{{"version":1,"agent_url":"a","traces":[{{"trace_id":"1","spans":[{{{span}}}]}}]}}"#
                ),
                "hex",
            ),
            (
                format!(
                    r#"{{"version":1,"agent_url":"a","traces":[{{{trace_id},"spans":[{{"span_id":"+000000000000002","service":"s","name":"n","start_unix_ns":5}}]}}]}}"#
                ),
                "hex",
            ),
            (
                format!(
                    r#"{{"version":1,"agent_url":"a","traces":[{{{trace_id},"spans":[{{{span},"attributes":{{"a":[1]}}}}]}}]}}"#
                ),
                "string, boolean or number",
            ),
            (
                format!(
                    r#"{{"version":1,"agent_url":"a","traces":[{{{trace_id},"spans":[{{{span},"attributes":{{"a":null}}}}]}}]}}"#
                ),
                "string, boolean or number",
            ),
        ] {
            let err = SpansToClose::parse(&block).unwrap_err().to_string();
            assert!(err.contains(why), "{block}: {err}");
        }
        let long = " ".repeat(SpansToClose::MAX_LEN + 1);
        assert!(SpansToClose::parse(&long).is_err());
    }
}
