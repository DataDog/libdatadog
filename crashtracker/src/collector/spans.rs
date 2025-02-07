// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;

use super::atomic_set::AtomicSpanSet;
use std::{io::Write, num::NonZeroU128};

static ACTIVE_SPANS: AtomicSpanSet<2048> = AtomicSpanSet::new();
static ACTIVE_TRACES: AtomicSpanSet<2048> = AtomicSpanSet::new();

pub fn clear_spans() -> anyhow::Result<()> {
    ACTIVE_SPANS.clear()
}

#[allow(dead_code)]
pub fn emit_spans(w: &mut impl Write) -> anyhow::Result<()> {
    use crate::shared::constants::*;
    writeln!(w, "{DD_CRASHTRACK_BEGIN_SPAN_IDS}")?;
    ACTIVE_SPANS.consume_and_emit(w, true)?;
    writeln!(w, "{DD_CRASHTRACK_END_SPAN_IDS}")?;
    w.flush()?;
    Ok(())
}

pub fn insert_span(value: u128) -> anyhow::Result<usize> {
    ACTIVE_SPANS.insert(NonZeroU128::new(value).context("Id of 0 not allowed")?)
}

pub fn remove_span(_value: u128, idx: usize) -> anyhow::Result<()> {
    ACTIVE_SPANS.remove(idx)
}

pub fn clear_traces() -> anyhow::Result<()> {
    ACTIVE_TRACES.clear()
}

#[allow(dead_code)]
pub fn emit_traces(w: &mut impl Write) -> anyhow::Result<()> {
    use crate::shared::constants::*;
    writeln!(w, "{DD_CRASHTRACK_BEGIN_TRACE_IDS}")?;
    ACTIVE_TRACES.consume_and_emit(w, true)?;
    writeln!(w, "{DD_CRASHTRACK_END_TRACE_IDS}")?;
    w.flush()?;
    Ok(())
}

pub fn insert_trace(value: u128) -> anyhow::Result<usize> {
    ACTIVE_TRACES.insert(NonZeroU128::new(value).context("A span with id 0 is not allowed")?)
}

pub fn remove_trace(_value: u128, idx: usize) -> anyhow::Result<()> {
    ACTIVE_TRACES.remove(idx)
}
