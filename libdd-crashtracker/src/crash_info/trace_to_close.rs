// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! APM traces a crash ends, staged by a tracer for the receiver to close and
//! submit to the agent. See [`TraceToClose`] for the wire format.

use base64::Engine;
use serde::Deserialize;
use std::collections::HashMap;

/// How a staged trace body is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceEncoding {
    /// A v1 tracer payload, `"v1"` on the wire, sent to `/v1.0/traces`.
    V1,
}

impl TraceEncoding {
    fn parse(name: &str) -> anyhow::Result<Self> {
        match name {
            "v1" => Ok(Self::V1),
            other => anyhow::bail!("unsupported encoding {other:?}"),
        }
    }

    /// The agent endpoint bodies of this encoding are sent to.
    pub fn agent_path(self) -> &'static str {
        match self {
            Self::V1 => "/v1.0/traces",
        }
    }
}

/// An APM trace the crash ends, staged by a tracer for the receiver to close
/// and submit to the agent.
///
/// # Wire format
///
/// Each trace is one block of exactly two lines, and a report may carry any
/// number of blocks, one per trace:
///
/// ```text
/// DD_CRASHTRACK_BEGIN_APM_TRACE_TO_CLOSE
/// {"version":1,"encoding":"v1","agent_url":"http://agent:8126","headers":{...},"span_ids":[...]}
/// <the request body, base64 (standard alphabet, padded)>
/// DD_CRASHTRACK_END_APM_TRACE_TO_CLOSE
/// ```
///
/// The first line is a JSON header:
///
/// | field       | required | meaning                                                         |
/// |-------------|----------|-----------------------------------------------------------------|
/// | `version`   | yes      | header schema version; [`TraceToClose::VERSION`]                |
/// | `encoding`  | yes      | body encoding, which also picks the agent endpoint; see [`TraceEncoding`] |
/// | `agent_url` | yes      | base agent URL, `http://` or `unix://`                          |
/// | `headers`   | no       | HTTP headers the tracer sends with traces (identity, origin, ...) |
/// | `span_ids`  | no       | the spans still open, which the crash ends; all spans if absent |
///
/// The second line is the body exactly as the tracer would POST it.
///
/// Unknown header fields are ignored, so collectors can add fields without
/// breaking receivers.
///
/// The SDK is trusted to stage a complete, correct trace: the receiver decodes
/// it, closes its spans and forwards it. A block it cannot read (an unknown
/// `version` or `encoding`, say) is not sent, and the report says why.
///
/// The tracer owns what is sent and where; the receiver owns what ending a
/// span at a crash means (see `receiver::traces_to_close`).
#[derive(Debug, Clone, PartialEq)]
pub struct TraceToClose {
    pub encoding: TraceEncoding,
    /// Base agent URL, `http://` or `unix://`.
    pub agent_url: String,
    /// HTTP headers the tracer sends with traces.
    pub headers: Vec<(String, String)>,
    /// The spans the crash ends; `None` ends every span in the body.
    pub span_ids: Option<Vec<u64>>,
    /// The request body, decoded from base64.
    pub body: Vec<u8>,
}

#[derive(Deserialize)]
struct Version {
    version: u32,
}

#[derive(Deserialize)]
struct HeaderV1 {
    encoding: String,
    agent_url: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    span_ids: Option<Vec<u64>>,
}

impl TraceToClose {
    /// The header schema version this receiver reads.
    pub const VERSION: u32 = 1;

    /// Parses a block's two lines: the JSON header and the base64 body.
    pub fn parse(header: &str, body: &str) -> anyhow::Result<Self> {
        let Version { version } =
            serde_json::from_str(header).map_err(|e| anyhow::anyhow!("header: {e}"))?;
        anyhow::ensure!(
            version == Self::VERSION,
            "unsupported header version {version}"
        );
        let header: HeaderV1 =
            serde_json::from_str(header).map_err(|e| anyhow::anyhow!("header: {e}"))?;
        let body = base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|e| anyhow::anyhow!("body: {e}"))?;
        Ok(Self {
            encoding: TraceEncoding::parse(&header.encoding)?,
            agent_url: header.agent_url,
            headers: header.headers.into_iter().collect(),
            span_ids: header.span_ids,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = r#"{"version":1,"encoding":"v1","agent_url":"unix:///var/run/datadog/apm.socket","headers":{"Datadog-Meta-Lang":"c"},"span_ids":[7,18446744073709551615]}"#;

    #[test]
    fn parses_every_field() {
        let t = TraceToClose::parse(HEADER, "CgP/").unwrap();
        assert_eq!(t.encoding, TraceEncoding::V1);
        assert_eq!(t.encoding.agent_path(), "/v1.0/traces");
        assert_eq!(t.agent_url, "unix:///var/run/datadog/apm.socket");
        assert_eq!(
            t.headers,
            vec![("Datadog-Meta-Lang".to_string(), "c".to_string())]
        );
        assert_eq!(t.span_ids, Some(vec![7, u64::MAX]));
        assert_eq!(t.body, vec![0x0a, 0x03, 0xff]);
    }

    #[test]
    fn optional_fields_default_and_unknown_fields_are_ignored() {
        let header =
            r#"{"version":1,"encoding":"v1","agent_url":"http://agent:8126","future":{"x":1}}"#;
        let t = TraceToClose::parse(header, "AQ==").unwrap();
        assert!(t.headers.is_empty());
        assert_eq!(t.span_ids, None);
        assert_eq!(t.body, vec![1]);
    }

    #[test]
    fn rejects_unknown_versions_and_encodings() {
        for header in [
            r#"{"version":2,"encoding":"v1","agent_url":"http://a"}"#,
            r#"{"version":1,"encoding":"v0.9","agent_url":"http://a"}"#,
        ] {
            assert!(TraceToClose::parse(header, "AQ==").is_err(), "{header}");
        }
    }
}
