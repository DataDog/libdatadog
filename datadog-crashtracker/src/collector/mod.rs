// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
mod additional_tags;
mod api;
mod atomic_set;
mod counters;
mod crash_handler;
mod emitters;
mod receiver_manager;
mod saguard;
mod signal_handler_manager;
mod spans;

pub use additional_tags::{
    clear_additional_tags, consume_and_emit_additional_tags, insert_additional_tag,
    remove_additional_tag,
};
pub use api::*;
pub use counters::{begin_op, end_op, reset_counters, OpTypes};
pub use crash_handler::{disable, enable, update_config, update_metadata};
pub use spans::{clear_spans, clear_traces, insert_span, insert_trace, remove_span, remove_trace};
