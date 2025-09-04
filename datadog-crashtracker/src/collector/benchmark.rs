// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]

use std::io::Write;
use crate::collector::emitters::{emit_crashreport, EmitterError};
use crate::collector::counters::{emit_counters, CounterError};
use crate::collector::spans::{emit_spans, emit_traces};
use crate::collector::additional_tags::consume_and_emit_additional_tags;
use crate::collector::atomic_set::AtomicSetError;
use crate::shared::configuration::CrashtrackerConfiguration;
use libc::{siginfo_t, ucontext_t};

// Expose internal emission functions for benchmarking
pub fn bench_emit_crashreport(
    pipe: &mut impl Write,
    config: &CrashtrackerConfiguration,
    config_str: &str,
    metadata_string: &str,
    sig_info: *const siginfo_t,
    ucontext: *const ucontext_t,
    ppid: i32,
) -> Result<(), EmitterError> {
    emit_crashreport(pipe, config, config_str, metadata_string, sig_info, ucontext, ppid)
}

pub fn bench_emit_counters(w: &mut impl Write) -> Result<(), CounterError> {
    emit_counters(w)
}

pub fn bench_emit_spans(w: &mut impl Write) -> Result<(), AtomicSetError> {
    emit_spans(w)
}

pub fn bench_emit_traces(w: &mut impl Write) -> Result<(), AtomicSetError> {
    emit_traces(w)
}

pub fn bench_consume_and_emit_additional_tags(w: &mut impl Write) -> Result<(), AtomicSetError> {
    consume_and_emit_additional_tags(w)
}

// Additional individual emission functions for benchmarking
pub fn bench_emit_metadata(w: &mut impl Write, metadata_str: &str) -> Result<(), EmitterError> {
    crate::collector::emitters::emit_metadata(w, metadata_str)
}

pub fn bench_emit_config(w: &mut impl Write, config_str: &str) -> Result<(), EmitterError> {
    crate::collector::emitters::emit_config(w, config_str)
}

pub fn bench_emit_siginfo(w: &mut impl Write, sig_info: *const siginfo_t) -> Result<(), EmitterError> {
    crate::collector::emitters::emit_siginfo(w, sig_info)
}

pub fn bench_emit_ucontext(w: &mut impl Write, ucontext: *const ucontext_t) -> Result<(), EmitterError> {
    crate::collector::emitters::emit_ucontext(w, ucontext)
}

pub fn bench_emit_procinfo(w: &mut impl Write, ppid: i32) -> Result<(), EmitterError> {
    crate::collector::emitters::emit_procinfo(w, ppid)
}
