// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! C ABI for `libdd-agent-client`.
//!
//! Exposes [`libdd_agent_client::AgentClient`] (and its builder, the
//! `LanguageMetadata`, `AgentInfo`, and `AgentResponse` types, plus the
//! six blocking send methods) through a stable C ABI suitable for any
//! C-compatible language. The first consumer is dd-trace-rb but the API
//! is not Ruby-specific.
//!
//! This crate intentionally reuses the shared types from
//! `libdd-http-client-ffi` — `DdogHttpClientError`, `DdogHttpHeader`,
//! `DdogHttpHeaderSlice`, `Handle<RetryConfig>`, `Handle<MultipartPart>` —
//! rather than redefining them. Callers should `#include
//! <datadog/http_client.h>` alongside `<datadog/agent_client.h>`.
//!
//! All exported symbols use the `ddog_` prefix and follow the conventions
//! established in `libdd-common-ffi` and `libdd-http-client-ffi`.

mod agent_info;
mod builder;
mod error;
mod language_metadata;
mod response;
mod send;
mod string_slice;
mod telemetry;

pub use agent_info::*;
pub use builder::*;
pub use error::*;
pub use language_metadata::*;
pub use response::*;
pub use send::*;
pub use string_slice::*;
pub use telemetry::*;
