// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
pub(crate) mod api;
mod counters;
pub(crate) mod crash_handler;
mod spans;
mod emitters;
pub use counters::{begin_profiling_op, end_profiling_op, reset_counters, ProfilingOpTypes};
pub use spans::{clear_spans, clear_traces, insert_span, insert_trace, remove_span, remove_trace};

