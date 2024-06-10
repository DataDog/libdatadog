// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Deserializer, Serialize};

fn deserialize_null_into_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    let opt = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

#[derive(Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SpanLink {
    /// @gotags: json:"trace_id" msg:"trace_id"
    ///
    /// Required.
    #[prost(uint64, tag = "1")]
    pub trace_id: u64,
    /// @gotags: json:"trace_id_high" msg:"trace_id_high,omitempty"
    ///
    /// Optional. The high 64 bits of a referenced trace id.
    #[prost(uint64, tag = "2")]
    pub trace_id_high: u64,
    /// @gotags: json:"span_id" msg:"span_id"
    ///
    /// Required.
    #[prost(uint64, tag = "3")]
    pub span_id: u64,
    /// @gotags: msg:"attributes,omitempty"
    ///
    /// Optional. Simple mapping of keys to string values.
    #[prost(map = "string, string", tag = "4")]
    pub attributes: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        ::prost::alloc::string::String,
    >,
    /// @gotags: msg:"tracestate,omitempty"
    ///
    /// Optional. W3C tracestate.
    #[prost(string, tag = "5")]
    pub tracestate: ::prost::alloc::string::String,
    /// @gotags: msg:"flags,omitempty"
    ///
    /// Optional. W3C trace flags. If set, the high bit (bit 31) must be set.
    #[prost(uint32, tag = "6")]
    pub flags: u32,
}
#[derive(Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Span {
    /// service is the name of the service with which this span is associated.
    /// @gotags: json:"service" msg:"service"
    #[prost(string, tag = "1")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    pub service: ::prost::alloc::string::String,
    /// name is the operation name of this span.
    /// @gotags: json:"name" msg:"name"
    #[prost(string, tag = "2")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    pub name: ::prost::alloc::string::String,
    /// resource is the resource name of this span, also sometimes called the endpoint (for web spans).
    /// @gotags: json:"resource" msg:"resource"
    #[prost(string, tag = "3")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    pub resource: ::prost::alloc::string::String,
    /// traceID is the ID of the trace to which this span belongs.
    /// @gotags: json:"trace_id" msg:"trace_id"
    #[prost(uint64, tag = "4")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    pub trace_id: u64,
    /// spanID is the ID of this span.
    /// @gotags: json:"span_id" msg:"span_id"
    #[prost(uint64, tag = "5")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    pub span_id: u64,
    /// parentID is the ID of this span's parent, or zero if this span has no parent.
    /// @gotags: json:"parent_id" msg:"parent_id"
    #[prost(uint64, tag = "6")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    pub parent_id: u64,
    /// start is the number of nanoseconds between the Unix epoch and the beginning of this span.
    /// @gotags: json:"start" msg:"start"
    #[prost(int64, tag = "7")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    pub start: i64,
    /// duration is the time length of this span in nanoseconds.
    /// @gotags: json:"duration" msg:"duration"
    #[prost(int64, tag = "8")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    pub duration: i64,
    /// error is 1 if there is an error associated with this span, or 0 if there is not.
    /// @gotags: json:"error" msg:"error"
    #[prost(int32, tag = "9")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    pub error: i32,
    /// meta is a mapping from tag name to tag value for string-valued tags.
    /// @gotags: json:"meta,omitempty" msg:"meta,omitempty"
    #[prost(map = "string, string", tag = "10")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    pub meta: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        ::prost::alloc::string::String,
    >,
    /// metrics is a mapping from tag name to tag value for numeric-valued tags.
    /// @gotags: json:"metrics,omitempty" msg:"metrics,omitempty"
    #[prost(map = "string, double", tag = "11")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    pub metrics: ::std::collections::HashMap<::prost::alloc::string::String, f64>,
    /// type is the type of the service with which this span is associated.  Example values: web, db, lambda.
    /// @gotags: json:"type" msg:"type"
    #[prost(string, tag = "12")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    pub r#type: ::prost::alloc::string::String,
    /// meta_struct is a registry of structured "other" data used by, e.g., AppSec.
    /// @gotags: json:"meta_struct,omitempty" msg:"meta_struct,omitempty"
    #[prost(map = "string, bytes", tag = "13")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    #[serde(skip_serializing_if = "::std::collections::HashMap::is_empty")]
    pub meta_struct: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        ::prost::alloc::vec::Vec<u8>,
    >,
    /// span_links represents a collection of links, where each link defines a causal relationship between two spans.
    /// @gotags: json:"span_links,omitempty" msg:"span_links,omitempty"
    #[prost(message, repeated, tag = "14")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_null_into_default")]
    #[serde(skip_serializing_if = "::prost::alloc::vec::Vec::is_empty")]
    pub span_links: ::prost::alloc::vec::Vec<SpanLink>,
}
/// TraceChunk represents a list of spans with the same trace ID. In other words, a chunk of a trace.
#[derive(Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TraceChunk {
    /// priority specifies sampling priority of the trace.
    /// @gotags: json:"priority" msg:"priority"
    #[prost(int32, tag = "1")]
    pub priority: i32,
    /// origin specifies origin product ("lambda", "rum", etc.) of the trace.
    /// @gotags: json:"origin" msg:"origin"
    #[prost(string, tag = "2")]
    pub origin: ::prost::alloc::string::String,
    /// spans specifies list of containing spans.
    /// @gotags: json:"spans" msg:"spans"
    #[prost(message, repeated, tag = "3")]
    pub spans: ::prost::alloc::vec::Vec<Span>,
    /// tags specifies tags common in all `spans`.
    /// @gotags: json:"tags" msg:"tags"
    #[prost(map = "string, string", tag = "4")]
    pub tags: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        ::prost::alloc::string::String,
    >,
    /// droppedTrace specifies whether the trace was dropped by samplers or not.
    /// @gotags: json:"dropped_trace" msg:"dropped_trace"
    #[prost(bool, tag = "5")]
    pub dropped_trace: bool,
}
/// TracerPayload represents a payload the trace agent receives from tracers.
#[derive(Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TracerPayload {
    /// containerID specifies the ID of the container where the tracer is running on.
    /// @gotags: json:"container_id" msg:"container_id"
    #[prost(string, tag = "1")]
    pub container_id: ::prost::alloc::string::String,
    /// languageName specifies language of the tracer.
    /// @gotags: json:"language_name" msg:"language_name"
    #[prost(string, tag = "2")]
    pub language_name: ::prost::alloc::string::String,
    /// languageVersion specifies language version of the tracer.
    /// @gotags: json:"language_version" msg:"language_version"
    #[prost(string, tag = "3")]
    pub language_version: ::prost::alloc::string::String,
    /// tracerVersion specifies version of the tracer.
    /// @gotags: json:"tracer_version" msg:"tracer_version"
    #[prost(string, tag = "4")]
    pub tracer_version: ::prost::alloc::string::String,
    /// runtimeID specifies V4 UUID representation of a tracer session.
    /// @gotags: json:"runtime_id" msg:"runtime_id"
    #[prost(string, tag = "5")]
    pub runtime_id: ::prost::alloc::string::String,
    /// chunks specifies list of containing trace chunks.
    /// @gotags: json:"chunks" msg:"chunks"
    #[prost(message, repeated, tag = "6")]
    pub chunks: ::prost::alloc::vec::Vec<TraceChunk>,
    /// tags specifies tags common in all `chunks`.
    /// @gotags: json:"tags" msg:"tags"
    #[prost(map = "string, string", tag = "7")]
    pub tags: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        ::prost::alloc::string::String,
    >,
    /// env specifies `env` tag that set with the tracer.
    /// @gotags: json:"env" msg:"env"
    #[prost(string, tag = "8")]
    pub env: ::prost::alloc::string::String,
    /// hostname specifies hostname of where the tracer is running.
    /// @gotags: json:"hostname" msg:"hostname"
    #[prost(string, tag = "9")]
    pub hostname: ::prost::alloc::string::String,
    /// version specifies `version` tag that set with the tracer.
    /// @gotags: json:"app_version" msg:"app_version"
    #[prost(string, tag = "10")]
    pub app_version: ::prost::alloc::string::String,
}
/// AgentPayload represents payload the agent sends to the intake.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AgentPayload {
    /// hostName specifies hostname of where the agent is running.
    #[prost(string, tag = "1")]
    pub host_name: ::prost::alloc::string::String,
    /// env specifies `env` set in agent configuration.
    #[prost(string, tag = "2")]
    pub env: ::prost::alloc::string::String,
    /// tracerPayloads specifies list of the payloads received from tracers.
    #[prost(message, repeated, tag = "5")]
    pub tracer_payloads: ::prost::alloc::vec::Vec<TracerPayload>,
    /// tags specifies tags common in all `tracerPayloads`.
    #[prost(map = "string, string", tag = "6")]
    pub tags: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        ::prost::alloc::string::String,
    >,
    /// agentVersion specifies version of the agent.
    #[prost(string, tag = "7")]
    pub agent_version: ::prost::alloc::string::String,
    /// targetTPS holds `TargetTPS` value in AgentConfig.
    #[prost(double, tag = "8")]
    pub target_tps: f64,
    /// errorTPS holds `ErrorTPS` value in AgentConfig.
    #[prost(double, tag = "9")]
    pub error_tps: f64,
    /// rareSamplerEnabled holds `RareSamplerEnabled` value in AgentConfig
    #[prost(bool, tag = "10")]
    pub rare_sampler_enabled: bool,
}
/// StatsPayload is the payload used to send stats from the agent to the backend.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct StatsPayload {
    #[prost(string, tag = "1")]
    pub agent_hostname: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub agent_env: ::prost::alloc::string::String,
    /// @gotags: json:"stats,omitempty" msg:"Stats,omitempty"
    #[prost(message, repeated, tag = "3")]
    pub stats: ::prost::alloc::vec::Vec<ClientStatsPayload>,
    #[prost(string, tag = "4")]
    pub agent_version: ::prost::alloc::string::String,
    #[prost(bool, tag = "5")]
    pub client_computed: bool,
    /// splitPayload indicates if the payload is actually one of several payloads split out from a larger payload.
    /// This field can be used in the backend to signal if re-aggregation is necessary.
    #[prost(bool, tag = "6")]
    pub split_payload: bool,
}
/// ClientStatsPayload is the first layer of span stats aggregation. It is also
/// the payload sent by tracers to the agent when stats in tracer are enabled.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ClientStatsPayload {
    /// Hostname is the tracer hostname. It's extracted from spans with "_dd.hostname" meta
    /// or set by tracer stats payload when hostname reporting is enabled.
    #[prost(string, tag = "1")]
    #[serde(default)]
    pub hostname: ::prost::alloc::string::String,
    /// env tag set on spans or in the tracers, used for aggregation
    #[prost(string, tag = "2")]
    #[serde(default)]
    pub env: ::prost::alloc::string::String,
    /// version tag set on spans or in the tracers, used for aggregation
    #[prost(string, tag = "3")]
    #[serde(default)]
    pub version: ::prost::alloc::string::String,
    /// @gotags: json:"stats,omitempty" msg:"Stats,omitempty"
    #[prost(message, repeated, tag = "4")]
    #[serde(default)]
    pub stats: ::prost::alloc::vec::Vec<ClientStatsBucket>,
    /// informative field not used for aggregation
    #[prost(string, tag = "5")]
    #[serde(default)]
    pub lang: ::prost::alloc::string::String,
    /// informative field not used for aggregation
    #[prost(string, tag = "6")]
    #[serde(default)]
    pub tracer_version: ::prost::alloc::string::String,
    /// used on stats payloads sent by the tracer to identify uniquely a message
    #[prost(string, tag = "7")]
    #[serde(default)]
    #[serde(rename = "RuntimeID")]
    pub runtime_id: ::prost::alloc::string::String,
    /// used on stats payloads sent by the tracer to identify uniquely a message
    #[prost(uint64, tag = "8")]
    #[serde(default)]
    pub sequence: u64,
    /// AgentAggregation is set by the agent on tracer payloads modified by the agent aggregation layer
    /// characterizes counts only and distributions only payloads
    #[prost(string, tag = "9")]
    #[serde(default)]
    pub agent_aggregation: ::prost::alloc::string::String,
    /// Service is the main service of the tracer.
    /// It is part of unified tagging: <https://docs.datadoghq.com/getting_started/tagging/unified_service_tagging>
    #[prost(string, tag = "10")]
    #[serde(default)]
    pub service: ::prost::alloc::string::String,
    /// ContainerID specifies the origin container ID. It is meant to be populated by the client and may
    /// be enhanced by the agent to ensure it is unique.
    #[prost(string, tag = "11")]
    #[serde(default)]
    #[serde(rename = "ContainerID")]
    pub container_id: ::prost::alloc::string::String,
    /// Tags specifies a set of tags obtained from the orchestrator (where applicable) using the specified containerID.
    /// This field should be left empty by the client. It only applies to some specific environment.
    #[prost(string, repeated, tag = "12")]
    #[serde(default)]
    pub tags: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    /// The git commit SHA is obtained from a trace, where it may be set through a tracer <-> source code integration.
    #[prost(string, tag = "13")]
    #[serde(default)]
    pub git_commit_sha: ::prost::alloc::string::String,
    /// The image tag is obtained from a container's set of tags.
    #[prost(string, tag = "14")]
    #[serde(default)]
    pub image_tag: ::prost::alloc::string::String,
}
/// ClientStatsBucket is a time bucket containing aggregated stats.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ClientStatsBucket {
    /// bucket start in nanoseconds
    #[prost(uint64, tag = "1")]
    pub start: u64,
    /// bucket duration in nanoseconds
    #[prost(uint64, tag = "2")]
    pub duration: u64,
    /// @gotags: json:"stats,omitempty" msg:"Stats,omitempty"
    #[prost(message, repeated, tag = "3")]
    pub stats: ::prost::alloc::vec::Vec<ClientGroupedStats>,
    /// AgentTimeShift is the shift applied by the agent stats aggregator on bucket start
    /// when the received bucket start is outside of the agent aggregation window
    #[prost(int64, tag = "4")]
    #[serde(default)]
    pub agent_time_shift: i64,
}
/// ClientGroupedStats aggregate stats on spans grouped by service, name, resource, status_code, type
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ClientGroupedStats {
    #[prost(string, tag = "1")]
    pub service: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub name: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub resource: ::prost::alloc::string::String,
    #[prost(uint32, tag = "4")]
    #[serde(rename = "HTTPStatusCode")]
    pub http_status_code: u32,
    #[prost(string, tag = "5")]
    #[serde(default)]
    pub r#type: ::prost::alloc::string::String,
    /// db_type might be used in the future to help in the obfuscation step
    #[prost(string, tag = "6")]
    #[serde(default)]
    #[serde(rename = "DBType")]
    pub db_type: ::prost::alloc::string::String,
    /// count of all spans aggregated in the groupedstats
    #[prost(uint64, tag = "7")]
    pub hits: u64,
    /// count of error spans aggregated in the groupedstats
    #[prost(uint64, tag = "8")]
    pub errors: u64,
    /// total duration in nanoseconds of spans aggregated in the bucket
    #[prost(uint64, tag = "9")]
    pub duration: u64,
    /// ddsketch summary of ok spans latencies encoded in protobuf
    #[prost(bytes = "vec", tag = "10")]
    #[serde(with = "serde_bytes")]
    pub ok_summary: ::prost::alloc::vec::Vec<u8>,
    /// ddsketch summary of error spans latencies encoded in protobuf
    #[prost(bytes = "vec", tag = "11")]
    #[serde(with = "serde_bytes")]
    pub error_summary: ::prost::alloc::vec::Vec<u8>,
    /// set to true on spans generated by synthetics traffic
    #[prost(bool, tag = "12")]
    pub synthetics: bool,
    /// count of top level spans aggregated in the groupedstats
    #[prost(uint64, tag = "13")]
    pub top_level_hits: u64,
    /// value of the span.kind tag on the span
    #[prost(string, tag = "15")]
    #[serde(default)]
    pub span_kind: ::prost::alloc::string::String,
    /// peer_tags are supplementary tags that further describe a peer entity
    /// E.g., `grpc.target` to describe the name of a gRPC peer, or `db.hostname` to describe the name of peer DB
    #[prost(string, repeated, tag = "16")]
    pub peer_tags: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    /// this field's value is equal to span's ParentID == 0.
    #[prost(enumeration = "Trilean", tag = "17")]
    pub is_trace_root: i32,
}
/// Trilean is an expanded boolean type that is meant to differentiate between being unset and false.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum Trilean {
    NotSet = 0,
    True = 1,
    False = 2,
}
impl Trilean {
    /// String value of the enum field names used in the ProtoBuf definition.
    ///
    /// The values are not transformed in any way and thus are considered stable
    /// (if the ProtoBuf definition does not change) and safe for programmatic use.
    pub fn as_str_name(&self) -> &'static str {
        match self {
            Trilean::NotSet => "NOT_SET",
            Trilean::True => "TRUE",
            Trilean::False => "FALSE",
        }
    }
    /// Creates an enum from field names used in the ProtoBuf definition.
    pub fn from_str_name(value: &str) -> ::core::option::Option<Self> {
        match value {
            "NOT_SET" => Some(Self::NotSet),
            "TRUE" => Some(Self::True),
            "FALSE" => Some(Self::False),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum TraceRootFlag {
    DeprecatedNotSet = 0,
    DeprecatedTrue = 1,
    DeprecatedFalse = 2,
}
impl TraceRootFlag {
    /// String value of the enum field names used in the ProtoBuf definition.
    ///
    /// The values are not transformed in any way and thus are considered stable
    /// (if the ProtoBuf definition does not change) and safe for programmatic use.
    pub fn as_str_name(&self) -> &'static str {
        match self {
            TraceRootFlag::DeprecatedNotSet => "DEPRECATED_NOT_SET",
            TraceRootFlag::DeprecatedTrue => "DEPRECATED_TRUE",
            TraceRootFlag::DeprecatedFalse => "DEPRECATED_FALSE",
        }
    }
    /// Creates an enum from field names used in the ProtoBuf definition.
    pub fn from_str_name(value: &str) -> ::core::option::Option<Self> {
        match value {
            "DEPRECATED_NOT_SET" => Some(Self::DeprecatedNotSet),
            "DEPRECATED_TRUE" => Some(Self::DeprecatedTrue),
            "DEPRECATED_FALSE" => Some(Self::DeprecatedFalse),
            _ => None,
        }
    }
}
