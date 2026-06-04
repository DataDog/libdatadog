// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This crate provides a Datadog-agent-specific HTTP client sitting on top of the basic
//! `libdd-http-client` primitives. The API is higher-level and makes agent-specific settings
//! (headers, etc.) the default rather than opt-in boilerplate.
//!
//! # Quick start
//!
//! ```rust,no_run
//! # fn example() -> Result<(), libdd_agent_client::BuildError> {
//! use libdd_agent_client::{AgentClient, LanguageMetadata};
//!
//! let client = AgentClient::builder()
//!     .http("localhost", 8126)
//!     .language_metadata(LanguageMetadata::new(
//!         "python", "3.12.1", "CPython", "", "2.18.0",
//!     ))
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! # Fork safety
//!
//! If this crate is used from a runtime that might fork, please enable the `reqwest-backend`
//! feature which use the `hickory-dns` DNS resolver by default. The latter is in-process and
//! fork-safe.

pub(crate) mod agent_info;
mod builder;
pub(crate) mod client;
pub(crate) mod error;
pub(crate) mod evp;
pub(crate) mod language_metadata;
pub(crate) mod telemetry;
pub(crate) mod traces;

pub use agent_info::AgentInfo;
pub use builder::{AgentClientBuilder, AgentTransport};
pub use client::AgentClient;
pub use error::{BuildError, SendError};
pub use evp::EvpEventRequest;
pub use language_metadata::LanguageMetadata;
pub use telemetry::TelemetryRequest;
pub use traces::{AgentResponse, TraceFormat, TraceSendOptions};
