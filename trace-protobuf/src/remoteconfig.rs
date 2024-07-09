// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct File {
    #[prost(string, tag = "1")]
    pub path: ::prost::alloc::string::String,
    #[prost(bytes = "vec", tag = "2")]
    #[serde(with = "serde_bytes")]
    pub raw: ::prost::alloc::vec::Vec<u8>,
}
#[derive(Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Client {
    #[prost(message, optional, tag = "1")]
    pub state: ::core::option::Option<ClientState>,
    #[prost(string, tag = "2")]
    pub id: ::prost::alloc::string::String,
    #[prost(string, repeated, tag = "3")]
    pub products: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    #[prost(bool, tag = "6")]
    pub is_tracer: bool,
    #[prost(message, optional, tag = "7")]
    pub client_tracer: ::core::option::Option<ClientTracer>,
    #[prost(bool, tag = "8")]
    pub is_agent: bool,
    #[prost(message, optional, tag = "9")]
    pub client_agent: ::core::option::Option<ClientAgent>,
    #[prost(uint64, tag = "10")]
    pub last_seen: u64,
    #[prost(bytes = "vec", tag = "11")]
    pub capabilities: ::prost::alloc::vec::Vec<u8>,
}
#[derive(Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ClientTracer {
    #[prost(string, tag = "1")]
    pub runtime_id: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub language: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub tracer_version: ::prost::alloc::string::String,
    #[prost(string, tag = "4")]
    pub service: ::prost::alloc::string::String,
    #[prost(string, repeated, tag = "8")]
    pub extra_services: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    #[prost(string, tag = "5")]
    pub env: ::prost::alloc::string::String,
    #[prost(string, tag = "6")]
    pub app_version: ::prost::alloc::string::String,
    #[prost(string, repeated, tag = "7")]
    pub tags: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
}
#[derive(Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ClientAgent {
    #[prost(string, tag = "1")]
    pub name: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub version: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub cluster_name: ::prost::alloc::string::String,
    #[prost(string, tag = "4")]
    pub cluster_id: ::prost::alloc::string::String,
    #[prost(string, repeated, tag = "5")]
    pub cws_workloads: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
}
#[derive(Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ConfigState {
    #[prost(string, tag = "1")]
    pub id: ::prost::alloc::string::String,
    #[prost(uint64, tag = "2")]
    pub version: u64,
    #[prost(string, tag = "3")]
    pub product: ::prost::alloc::string::String,
    #[prost(uint64, tag = "4")]
    pub apply_state: u64,
    #[prost(string, tag = "5")]
    pub apply_error: ::prost::alloc::string::String,
}
#[derive(Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ClientState {
    #[prost(uint64, tag = "1")]
    pub root_version: u64,
    #[prost(uint64, tag = "2")]
    pub targets_version: u64,
    #[prost(message, repeated, tag = "3")]
    pub config_states: ::prost::alloc::vec::Vec<ConfigState>,
    #[prost(bool, tag = "4")]
    pub has_error: bool,
    #[prost(string, tag = "5")]
    pub error: ::prost::alloc::string::String,
    #[prost(bytes = "vec", tag = "6")]
    pub backend_client_state: ::prost::alloc::vec::Vec<u8>,
}
#[derive(Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TargetFileHash {
    #[prost(string, tag = "1")]
    pub algorithm: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub hash: ::prost::alloc::string::String,
}
#[derive(Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TargetFileMeta {
    #[prost(string, tag = "1")]
    pub path: ::prost::alloc::string::String,
    #[prost(int64, tag = "2")]
    pub length: i64,
    #[prost(message, repeated, tag = "3")]
    pub hashes: ::prost::alloc::vec::Vec<TargetFileHash>,
}
#[derive(Deserialize, Serialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ClientGetConfigsRequest {
    #[prost(message, optional, tag = "1")]
    pub client: ::core::option::Option<Client>,
    #[prost(message, repeated, tag = "2")]
    pub cached_target_files: ::prost::alloc::vec::Vec<TargetFileMeta>,
}
#[derive(Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ClientGetConfigsResponse {
    #[prost(bytes = "vec", repeated, tag = "1")]
    #[serde(with = "crate::serde")]
    #[serde(default)]
    pub roots: ::prost::alloc::vec::Vec<::prost::alloc::vec::Vec<u8>>,
    #[prost(bytes = "vec", tag = "2")]
    #[serde(with = "serde_bytes")]
    #[serde(default)]
    pub targets: ::prost::alloc::vec::Vec<u8>,
    #[prost(message, repeated, tag = "3")]
    #[serde(default)]
    pub target_files: ::prost::alloc::vec::Vec<File>,
    #[prost(string, repeated, tag = "4")]
    #[serde(default)]
    pub client_configs: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
}
