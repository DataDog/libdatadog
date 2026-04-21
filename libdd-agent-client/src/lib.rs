// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This crate provides a Datadog-agent-specific HTTP client sitting on top of the basic
//! `libdd-http-client` primitives. The API is higher-level and makes agent-specific settings
//! (headers, etc.) the default rather than opt-in boilerplate.
//!
//! # Quick start
//!
//! Call [`AgentClientBuilder::auto_detect`] to let the client configure transport and timeout
//! from the standard Datadog environment variables (`DD_TRACE_AGENT_URL`, `DD_AGENT_HOST`,
//! `DD_TRACE_AGENT_PORT`, `DD_TRACE_AGENT_TIMEOUT_SECONDS`), falling back to a local Unix
//! socket at `/var/run/datadog/apm.socket` when it exists, and finally to `localhost:8126`.
//!
//! ```rust,no_run
//! # #[cfg(unix)]
//! # fn example() -> Result<(), libdd_agent_client::BuildError> {
//! use libdd_agent_client::{AgentClient, LanguageMetadata};
//!
//! let client = AgentClient::builder()
//!     .auto_detect()
//!     .language_metadata(LanguageMetadata::new("python", "3.12.1", "CPython", "2.18.0"))
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! # Explicit transport
//!
//! When the host and port are known at build time, set them directly:
//!
//! ```rust,no_run
//! # fn example() -> Result<(), libdd_agent_client::BuildError> {
//! use libdd_agent_client::{AgentClient, LanguageMetadata};
//!
//! let client = AgentClient::builder()
//!     .http("localhost", 8126)
//!     .language_metadata(LanguageMetadata::new("python", "3.12.1", "CPython", "2.18.0"))
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! # Fork safety
//!
//! The underlying `libdd-http-client` uses the `hickory-dns` DNS resolver by default, which is
//! in-process and fork-safe.

pub mod agent_info;
pub mod builder;
pub mod client;
pub mod error;
pub mod language_metadata;
pub mod telemetry;
pub mod traces;

pub use agent_info::AgentInfo;
pub use builder::{AgentClientBuilder, AgentTransport};
pub use client::AgentClient;
pub use error::{BuildError, SendError};
pub use language_metadata::LanguageMetadata;
pub use telemetry::TelemetryRequest;
pub use traces::{AgentResponse, TraceFormat, TraceSendOptions};
