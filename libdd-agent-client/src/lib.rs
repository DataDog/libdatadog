// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! `libdd-agent-client` — Datadog-agent-specialised HTTP client.
//!
//! This crate is **Milestone 2** of APMSP-2721 (libdd-http-client: Common HTTP Client for
//! Language Clients). It sits on top of the basic `libdd-http-client` primitives (Milestone 1)
//! and encapsulates all Datadog-agent-specific concerns, making them the **default** rather than
//! opt-in boilerplate that every subsystem must repeat.
//!
//! # What it replaces in dd-trace-py
//!
//! | Concern | dd-trace-py location |
//! |---------|----------------------|
//! | Language metadata headers | `writer.py:638-644`, `stats.py:113-117`, `datastreams/processor.py:128-133` |
//! | Container/entity-ID headers | `http.py:32-37`, `container.py:157-183` |
//! | Retry logic (fibonacci backoff) | `writer.py:245-249`, `stats.py:123-126`, `datastreams/processor.py:140-143` |
//! | Trace send with `X-Datadog-Trace-Count` | `writer.py:749-752` |
//! | `rate_by_service` parsing | `writer.py:728-734` |
//! | Stats send | `stats.py:204-228` |
//! | Pipeline stats send (always gzip) | `datastreams/processor.py:204-210` |
//! | Telemetry send with per-request headers | `telemetry/writer.py:111-129` |
//! | EVP event send | `openfeature/writer.py:114-117` |
//! | `GET /info` with typed result | `agent.py:17-46` |
//!
//! # Quick start
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), libdd_agent_client::BuildError> {
//! use libdd_agent_client::{AgentClient, LanguageMetadata};
//!
//! let client = AgentClient::builder()
//!     .http("localhost", 8126)
//!     .language_metadata(LanguageMetadata::new(
//!         "python", "3.12.1", "CPython", "2.18.0",
//!     ))
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! # Unix Domain Socket
//!
//! ```rust,no_run
//! # #[cfg(unix)]
//! # async fn example() -> Result<(), libdd_agent_client::BuildError> {
//! use libdd_agent_client::{AgentClient, LanguageMetadata};
//!
//! let client = AgentClient::builder()
//!     .unix_socket("/var/run/datadog/apm.socket")
//!     .language_metadata(LanguageMetadata::new(
//!         "python", "3.12.1", "CPython", "2.18.0",
//!     ))
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! # Fork safety
//!
//! The underlying `libdd-http-client` uses `hickory-dns` by default — an in-process, fork-safe
//! DNS resolver. This protects against the class of DNS bugs that can occur in forking processes
//! (Django workers, Celery, PHP-FPM, etc.).

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
