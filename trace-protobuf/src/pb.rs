// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Span {
    /// service is the name of the service with which this span is associated.
    #[prost(string, tag = "1")]
    #[serde(default)]
    pub service: ::prost::alloc::string::String,
    /// name is the operation name of this span.
    #[prost(string, tag = "2")]
    pub name: ::prost::alloc::string::String,
    /// resource is the resource name of this span, also sometimes called the endpoint (for web spans).
    #[prost(string, tag = "3")]
    pub resource: ::prost::alloc::string::String,
    /// traceID is the ID of the trace to which this span belongs.
    #[prost(uint64, tag = "4")]
    pub trace_id: u64,
    /// spanID is the ID of this span.
    #[prost(uint64, tag = "5")]
    pub span_id: u64,
    /// parentID is the ID of this span's parent, or zero if this span has no parent.
    #[prost(uint64, tag = "6")]
    #[serde(default)]
    pub parent_id: u64,
    /// start is the number of nanoseconds between the Unix epoch and the beginning of this span.
    #[prost(int64, tag = "7")]
    pub start: i64,
    /// duration is the time length of this span in nanoseconds.
    #[prost(int64, tag = "8")]
    pub duration: i64,
    /// error is 1 if there is an error associated with this span, or 0 if there is not.
    #[prost(int32, tag = "9")]
    #[serde(default)]
    pub error: i32,
    /// meta is a mapping from tag name to tag value for string-valued tags.
    #[prost(map = "string, string", tag = "10")]
    pub meta: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        ::prost::alloc::string::String,
    >,
    /// metrics is a mapping from tag name to tag value for numeric-valued tags.
    #[prost(map = "string, double", tag = "11")]
    #[serde(default)]
    pub metrics: ::std::collections::HashMap<::prost::alloc::string::String, f64>,
    /// type is the type of the service with which this span is associated.  Example values: web, db, lambda.
    #[prost(string, tag = "12")]
    #[serde(default)]
    pub r#type: ::prost::alloc::string::String,
    /// meta_struct is a registry of structured "other" data used by, e.g., AppSec.
    #[prost(map = "string, bytes", tag = "13")]
    #[serde(default)]
    pub meta_struct: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        ::prost::alloc::vec::Vec<u8>,
    >,
}
/// TraceChunk represents a list of spans with the same trace ID. In other words, a chunk of a trace.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TraceChunk {
    /// priority specifies sampling priority of the trace.
    #[prost(int32, tag = "1")]
    pub priority: i32,
    /// origin specifies origin product ("lambda", "rum", etc.) of the trace.
    #[prost(string, tag = "2")]
    pub origin: ::prost::alloc::string::String,
    /// spans specifies list of containing spans.
    #[prost(message, repeated, tag = "3")]
    pub spans: ::prost::alloc::vec::Vec<Span>,
    /// tags specifies tags common in all `spans`.
    #[prost(map = "string, string", tag = "4")]
    pub tags: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        ::prost::alloc::string::String,
    >,
    /// droppedTrace specifies whether the trace was dropped by samplers or not.
    #[prost(bool, tag = "5")]
    pub dropped_trace: bool,
}
/// TracerPayload represents a payload the trace agent receives from tracers.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TracerPayload {
    /// containerID specifies the ID of the container where the tracer is running on.
    #[prost(string, tag = "1")]
    pub container_id: ::prost::alloc::string::String,
    /// languageName specifies language of the tracer.
    #[prost(string, tag = "2")]
    pub language_name: ::prost::alloc::string::String,
    /// languageVersion specifies language version of the tracer.
    #[prost(string, tag = "3")]
    pub language_version: ::prost::alloc::string::String,
    /// tracerVersion specifies version of the tracer.
    #[prost(string, tag = "4")]
    pub tracer_version: ::prost::alloc::string::String,
    /// runtimeID specifies V4 UUID representation of a tracer session.
    #[prost(string, tag = "5")]
    pub runtime_id: ::prost::alloc::string::String,
    /// chunks specifies list of containing trace chunks.
    #[prost(message, repeated, tag = "6")]
    pub chunks: ::prost::alloc::vec::Vec<TraceChunk>,
    /// tags specifies tags common in all `chunks`.
    #[prost(map = "string, string", tag = "7")]
    pub tags: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        ::prost::alloc::string::String,
    >,
    /// env specifies `env` tag that set with the tracer.
    #[prost(string, tag = "8")]
    pub env: ::prost::alloc::string::String,
    /// hostname specifies hostname of where the tracer is running.
    #[prost(string, tag = "9")]
    pub hostname: ::prost::alloc::string::String,
    /// version specifies `version` tag that set with the tracer.
    #[prost(string, tag = "10")]
    pub app_version: ::prost::alloc::string::String,
}
/// AgentPayload represents payload the agent sends to the intake.
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
    #[prost(message, repeated, tag = "3")]
    pub stats: ::prost::alloc::vec::Vec<ClientStatsPayload>,
    #[prost(string, tag = "4")]
    pub agent_version: ::prost::alloc::string::String,
    #[prost(bool, tag = "5")]
    pub client_computed: bool,
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
    pub hostname: ::prost::alloc::string::String,
    /// env tag set on spans or in the tracers, used for aggregation
    #[prost(string, tag = "2")]
    pub env: ::prost::alloc::string::String,
    /// version tag set on spans or in the tracers, used for aggregation
    #[prost(string, tag = "3")]
    #[serde(default)]
    pub version: ::prost::alloc::string::String,
    #[prost(message, repeated, tag = "4")]
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
}
