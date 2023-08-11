// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ConfigMetas {
    #[prost(message, repeated, tag = "1")]
    pub roots: ::prost::alloc::vec::Vec<TopMeta>,
    #[prost(message, optional, tag = "2")]
    pub timestamp: ::core::option::Option<TopMeta>,
    #[prost(message, optional, tag = "3")]
    pub snapshot: ::core::option::Option<TopMeta>,
    #[prost(message, optional, tag = "4")]
    pub top_targets: ::core::option::Option<TopMeta>,
    #[prost(message, repeated, tag = "5")]
    pub delegated_targets: ::prost::alloc::vec::Vec<DelegatedMeta>,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct DirectorMetas {
    #[prost(message, repeated, tag = "1")]
    pub roots: ::prost::alloc::vec::Vec<TopMeta>,
    #[prost(message, optional, tag = "2")]
    pub timestamp: ::core::option::Option<TopMeta>,
    #[prost(message, optional, tag = "3")]
    pub snapshot: ::core::option::Option<TopMeta>,
    #[prost(message, optional, tag = "4")]
    pub targets: ::core::option::Option<TopMeta>,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct DelegatedMeta {
    #[prost(uint64, tag = "1")]
    pub version: u64,
    #[prost(string, tag = "2")]
    pub role: ::prost::alloc::string::String,
    #[prost(bytes = "vec", tag = "3")]
    pub raw: ::prost::alloc::vec::Vec<u8>,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TopMeta {
    #[prost(uint64, tag = "1")]
    pub version: u64,
    #[prost(bytes = "vec", tag = "2")]
    pub raw: ::prost::alloc::vec::Vec<u8>,
}
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
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LatestConfigsRequest {
    #[prost(string, tag = "1")]
    pub hostname: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub agent_version: ::prost::alloc::string::String,
    /// timestamp and snapshot versions move in tandem so they are the same.
    #[prost(uint64, tag = "3")]
    pub current_config_snapshot_version: u64,
    #[prost(uint64, tag = "9")]
    pub current_config_root_version: u64,
    #[prost(uint64, tag = "8")]
    pub current_director_root_version: u64,
    #[prost(string, repeated, tag = "4")]
    pub products: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    #[prost(string, repeated, tag = "5")]
    pub new_products: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    #[prost(message, repeated, tag = "6")]
    pub active_clients: ::prost::alloc::vec::Vec<Client>,
    #[prost(bytes = "vec", tag = "10")]
    pub backend_client_state: ::prost::alloc::vec::Vec<u8>,
    #[prost(bool, tag = "11")]
    pub has_error: bool,
    #[prost(string, tag = "12")]
    pub error: ::prost::alloc::string::String,
    #[prost(string, tag = "13")]
    pub trace_agent_env: ::prost::alloc::string::String,
    #[prost(string, tag = "14")]
    pub org_uuid: ::prost::alloc::string::String,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LatestConfigsResponse {
    #[prost(message, optional, tag = "1")]
    pub config_metas: ::core::option::Option<ConfigMetas>,
    #[prost(message, optional, tag = "2")]
    pub director_metas: ::core::option::Option<DirectorMetas>,
    #[prost(message, repeated, tag = "3")]
    pub target_files: ::prost::alloc::vec::Vec<File>,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct OrgDataResponse {
    #[prost(string, tag = "1")]
    pub uuid: ::prost::alloc::string::String,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct OrgStatusResponse {
    #[prost(bool, tag = "1")]
    pub enabled: bool,
    #[prost(bool, tag = "2")]
    pub authorized: bool,
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
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FileMetaState {
    #[prost(uint64, tag = "1")]
    pub version: u64,
    #[prost(string, tag = "2")]
    pub hash: ::prost::alloc::string::String,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GetStateConfigResponse {
    #[prost(map = "string, message", tag = "1")]
    pub config_state: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        FileMetaState,
    >,
    #[prost(map = "string, message", tag = "2")]
    pub director_state: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        FileMetaState,
    >,
    #[prost(map = "string, string", tag = "3")]
    pub target_filenames: ::std::collections::HashMap<
        ::prost::alloc::string::String,
        ::prost::alloc::string::String,
    >,
    #[prost(message, repeated, tag = "4")]
    pub active_clients: ::prost::alloc::vec::Vec<Client>,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TracerPredicateV1 {
    #[prost(string, tag = "1")]
    pub client_id: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub service: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub environment: ::prost::alloc::string::String,
    #[prost(string, tag = "4")]
    pub app_version: ::prost::alloc::string::String,
    #[prost(string, tag = "5")]
    pub tracer_version: ::prost::alloc::string::String,
    #[prost(string, tag = "6")]
    pub language: ::prost::alloc::string::String,
    #[prost(string, tag = "7")]
    pub runtime_id: ::prost::alloc::string::String,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TracerPredicates {
    #[prost(message, repeated, tag = "1")]
    pub tracer_predicates_v1: ::prost::alloc::vec::Vec<TracerPredicateV1>,
}
