// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_name: Option<String>,
}

/// A pre-encoded trace payload the collector staged before the crash: the
/// final version of spans the crash ends (for dd-trace-c, the `training.rank`
/// span). The receiver stamps the crash into it and submits it to the agent.
///
/// Wire format, between `DD_CRASHTRACK_BEGIN_APM_TRACE_PAYLOAD` and
/// `DD_CRASHTRACK_END_APM_TRACE_PAYLOAD`, one `key=value` per line. Unknown
/// keys are ignored, so collectors can add keys without breaking receivers:
///
/// | key             | value                                                  |
/// |-----------------|--------------------------------------------------------|
/// | `format`        | payload encoding; `v1` (agent `/v1.0/traces`)          |
/// | `crash_unix_ns` | wall-clock crash time in ns, the spans' new end        |
/// | `agent_url`     | where the tracer sends traces (`http://`, `unix://`)   |
/// | `payload`       | the payload, hex-encoded                               |
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ApmTracePayload {
    /// Encoding of `data`, e.g. [`ApmTracePayload::FORMAT_V1`].
    pub format: String,
    /// When the process crashed, in nanoseconds since the Unix epoch.
    pub crash_unix_ns: Option<u64>,
    /// The agent URL the tracer sends traces to, if the collector sent one.
    pub agent_url: Option<String>,
    /// The payload bytes, hex-decoded.
    pub data: Vec<u8>,
}

impl ApmTracePayload {
    /// A v1 tracer payload, as sent to the agent's `/v1.0/traces`.
    pub const FORMAT_V1: &'static str = "v1";

    /// Applies one `key=value` line of the block. Unknown keys are ignored.
    pub fn apply_line(&mut self, line: &str) -> anyhow::Result<()> {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("expected key=value"))?;
        match key {
            "format" => self.format = value.to_string(),
            "crash_unix_ns" => self.crash_unix_ns = Some(value.parse()?),
            "agent_url" if !value.is_empty() => self.agent_url = Some(value.to_string()),
            "payload" => self.data = hex::decode(value)?,
            _ => {}
        }
        Ok(())
    }

    /// Why the block cannot be submitted, if it cannot.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.data.is_empty(), "no payload");
        anyhow::ensure!(
            self.format == Self::FORMAT_V1,
            "unsupported format {:?}",
            self.format
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_line_reads_every_key() {
        let mut p = ApmTracePayload::default();
        for line in [
            "format=v1",
            "crash_unix_ns=1700000000123456789",
            "agent_url=unix:///var/run/datadog/apm.socket",
            "payload=0a0bff",
        ] {
            p.apply_line(line).unwrap();
        }
        assert_eq!(p.format, "v1");
        assert_eq!(p.crash_unix_ns, Some(1_700_000_000_123_456_789));
        assert_eq!(
            p.agent_url.as_deref(),
            Some("unix:///var/run/datadog/apm.socket")
        );
        assert_eq!(p.data, vec![0x0a, 0x0b, 0xff]);
        p.validate().unwrap();
    }

    #[test]
    fn apply_line_ignores_unknown_keys_and_keeps_equals_in_values() {
        let mut p = ApmTracePayload::default();
        p.apply_line("future_key=whatever").unwrap();
        p.apply_line("agent_url=http://h:8126/?a=b").unwrap();
        assert_eq!(p.agent_url.as_deref(), Some("http://h:8126/?a=b"));
    }

    #[test]
    fn apply_line_rejects_malformed_lines() {
        let mut p = ApmTracePayload::default();
        assert!(p.apply_line("no separator").is_err());
        assert!(p.apply_line("payload=zz").is_err());
        assert!(p.apply_line("crash_unix_ns=-1").is_err());
    }

    #[test]
    fn validate_needs_a_known_format_and_a_payload() {
        let mut p = ApmTracePayload::default();
        assert!(p.validate().is_err());
        p.data = vec![1];
        assert!(p.validate().is_err());
        p.format = "v0.4".to_string();
        assert!(p.validate().is_err());
        p.format = "v1".to_string();
        p.validate().unwrap();
    }
}
