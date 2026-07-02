// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// TEMP (DO NOT MERGE): trivial change to exercise PR benchmark crate scoping.

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod agentless_encoder;
pub mod config_utils;
pub mod json_log_encoder;
pub mod msgpack_decoder;
pub mod msgpack_encoder;
pub mod otlp_encoder;
pub mod send_data;
pub mod send_with_retry;
pub mod stats_utils;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
pub mod trace_filter;
pub mod trace_utils;
pub mod tracer_header_tags;
pub mod tracer_metadata;
pub mod tracer_payload;

pub mod span;

#[cfg(feature = "change-buffer")]
pub mod change_buffer;
