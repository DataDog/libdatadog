// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
mod api;
mod counters;
mod crash_handler;
mod emitters;
mod saguard;
mod spans;
mod alt_fork;

pub use api::*;
pub use counters::{begin_op, end_op, reset_counters, OpTypes};
pub use crash_handler::{update_config, update_metadata};
pub use spans::{clear_spans, clear_traces, insert_span, insert_trace, remove_span, remove_trace};
