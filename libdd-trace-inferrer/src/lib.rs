// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! # libdd-trace-inferrer
//!
//! A library for inferring Datadog trace spans from JSON payloads.
//!
//! This crate identifies AWS Lambda trigger event payloads and extracts span
//! data, trace context carriers, trigger tags, span pointers, and more. It is
//! designed to be consumed by any tracer runtime (Rust, Ruby, Python, etc.)
//! through an FFI layer.
//!
//! ## Quick start
//!
//! ```rust
//! use libdd_trace_inferrer::{SpanInferrer, InferConfig};
//!
//! let config = InferConfig::default();
//! let inferrer = SpanInferrer::new(config);
//!
//! let payload = r#"{"version":"2.0","rawQueryString":"","requestContext":{"domainName":"api.example.com","http":{"method":"GET","path":"/test"}}}"#;
//! let result = inferrer.infer_span(payload);
//! ```

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

mod config;
mod error;
mod inferrer;
mod span_data;
mod span_pointer;
pub mod triggers;
mod utils;

pub use config::InferConfig;
pub use error::InferrerError;
pub use inferrer::{
    CompletedSpan, CompletedSpans, CompletionContext, InferenceResult, SpanInferrer,
    complete_inference,
};
pub use span_data::SpanData;
pub use span_pointer::SpanPointer;
pub use triggers::TriggerType;
