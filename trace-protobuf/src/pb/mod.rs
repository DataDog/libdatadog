// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Span {
    /// service is the name of the service with which this span is associated.
    #[prost(string, tag="1")]
    pub service: std::string::String,
    /// name is the operation name of this span.
    #[prost(string, tag="2")]
    pub name: std::string::String,
    /// resource is the resource name of this span, also sometimes called the endpoint (for web spans).
    #[prost(string, tag="3")]
    pub resource: std::string::String,
    /// traceID is the ID of the trace to which this span belongs.
    #[prost(uint64, tag="4")]
    pub trace_id: u64,
    /// spanID is the ID of this span.
    #[prost(uint64, tag="5")]
    pub span_id: u64,
    /// parentID is the ID of this span's parent, or zero if this span has no parent.
    #[prost(uint64, tag="6")]
    pub parent_id: u64,
    /// start is the number of nanoseconds between the Unix epoch and the beginning of this span.
    #[prost(int64, tag="7")]
    pub start: i64,
    /// duration is the time length of this span in nanoseconds.
    #[prost(int64, tag="8")]
    pub duration: i64,
    /// error is 1 if there is an error associated with this span, or 0 if there is not.
    #[prost(int32, tag="9")]
    pub error: i32,
    /// meta is a mapping from tag name to tag value for string-valued tags.
    #[prost(map="string, string", tag="10")]
    pub meta: ::std::collections::HashMap<std::string::String, std::string::String>,
    /// metrics is a mapping from tag name to tag value for numeric-valued tags.
    #[prost(map="string, double", tag="11")]
    pub metrics: ::std::collections::HashMap<std::string::String, f64>,
    /// type is the type of the service with which this span is associated.  Example values: web, db, lambda.
    #[prost(string, tag="12")]
    pub r#type: std::string::String,
    /// meta_struct is a registry of structured "other" data used by, e.g., AppSec.
    #[prost(map="string, bytes", tag="13")]
    pub meta_struct: ::std::collections::HashMap<std::string::String, std::vec::Vec<u8>>,
}
/// TraceChunk represents a list of spans with the same trace ID. In other words, a chunk of a trace.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TraceChunk {
    /// priority specifies sampling priority of the trace.
    #[prost(int32, tag="1")]
    pub priority: i32,
    /// origin specifies origin product ("lambda", "rum", etc.) of the trace.
    #[prost(string, tag="2")]
    pub origin: std::string::String,
    /// spans specifies list of containing spans.
    #[prost(message, repeated, tag="3")]
    pub spans: ::std::vec::Vec<Span>,
    /// tags specifies tags common in all `spans`.
    #[prost(map="string, string", tag="4")]
    pub tags: ::std::collections::HashMap<std::string::String, std::string::String>,
    /// droppedTrace specifies whether the trace was dropped by samplers or not.
    #[prost(bool, tag="5")]
    pub dropped_trace: bool,
}
/// TracerPayload represents a payload the trace agent receives from tracers.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TracerPayload {
    /// containerID specifies the ID of the container where the tracer is running on.
    #[prost(string, tag="1")]
    pub container_id: std::string::String,
    /// languageName specifies language of the tracer.
    #[prost(string, tag="2")]
    pub language_name: std::string::String,
    /// languageVersion specifies language version of the tracer.
    #[prost(string, tag="3")]
    pub language_version: std::string::String,
    /// tracerVersion specifies version of the tracer.
    #[prost(string, tag="4")]
    pub tracer_version: std::string::String,
    /// runtimeID specifies V4 UUID representation of a tracer session.
    #[prost(string, tag="5")]
    pub runtime_id: std::string::String,
    /// chunks specifies list of containing trace chunks.
    #[prost(message, repeated, tag="6")]
    pub chunks: ::std::vec::Vec<TraceChunk>,
    /// tags specifies tags common in all `chunks`.
    #[prost(map="string, string", tag="7")]
    pub tags: ::std::collections::HashMap<std::string::String, std::string::String>,
    /// env specifies `env` tag that set with the tracer.
    #[prost(string, tag="8")]
    pub env: std::string::String,
    /// hostname specifies hostname of where the tracer is running.
    #[prost(string, tag="9")]
    pub hostname: std::string::String,
    /// version specifies `version` tag that set with the tracer.
    #[prost(string, tag="10")]
    pub app_version: std::string::String,
}
/// AgentPayload represents payload the agent sends to the intake.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AgentPayload {
    /// hostName specifies hostname of where the agent is running.
    #[prost(string, tag="1")]
    pub host_name: std::string::String,
    /// env specifies `env` set in agent configuration.
    #[prost(string, tag="2")]
    pub env: std::string::String,
    /// tracerPayloads specifies list of the payloads received from tracers.
    #[prost(message, repeated, tag="5")]
    pub tracer_payloads: ::std::vec::Vec<TracerPayload>,
    /// tags specifies tags common in all `tracerPayloads`.
    #[prost(map="string, string", tag="6")]
    pub tags: ::std::collections::HashMap<std::string::String, std::string::String>,
    /// agentVersion specifies version of the agent.
    #[prost(string, tag="7")]
    pub agent_version: std::string::String,
    /// targetTPS holds `TargetTPS` value in AgentConfig.
    #[prost(double, tag="8")]
    pub target_tps: f64,
    /// errorTPS holds `ErrorTPS` value in AgentConfig.
    #[prost(double, tag="9")]
    pub error_tps: f64,
}