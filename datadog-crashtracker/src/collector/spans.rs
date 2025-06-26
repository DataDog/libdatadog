// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::atomic_set::{AtomicSetError, AtomicSpanSet};
use std::{io::Write, num::NonZeroU128};

static ACTIVE_SPANS: AtomicSpanSet<2048> = AtomicSpanSet::new();
static ACTIVE_TRACES: AtomicSpanSet<2048> = AtomicSpanSet::new();

pub fn clear_spans() -> Result<(), AtomicSetError> {
    ACTIVE_SPANS.clear()
}

#[allow(dead_code)]
pub fn emit_spans(w: &mut impl Write) -> Result<(), AtomicSetError> {
    use crate::shared::constants::*;
    writeln!(w, "{DD_CRASHTRACK_BEGIN_SPAN_IDS}")?;
    ACTIVE_SPANS.consume_and_emit(w, true)?;
    writeln!(w, "{DD_CRASHTRACK_END_SPAN_IDS}")?;
    w.flush()?;
    Ok(())
}

pub fn insert_span(value: u128) -> Result<usize, AtomicSetError> {
    let non_zero = NonZeroU128::new(value)
        .ok_or_else(|| AtomicSetError::InvalidValue("Id of 0 not allowed".to_string()))?;
    ACTIVE_SPANS.insert(non_zero)
}

pub fn remove_span(_value: u128, idx: usize) -> Result<(), AtomicSetError> {
    ACTIVE_SPANS.remove(idx)
}

pub fn clear_traces() -> Result<(), AtomicSetError> {
    ACTIVE_TRACES.clear()
}

#[allow(dead_code)]
pub fn emit_traces(w: &mut impl Write) -> Result<(), AtomicSetError> {
    use crate::shared::constants::*;
    writeln!(w, "{DD_CRASHTRACK_BEGIN_TRACE_IDS}")?;
    ACTIVE_TRACES.consume_and_emit(w, true)?;
    writeln!(w, "{DD_CRASHTRACK_END_TRACE_IDS}")?;
    w.flush()?;
    Ok(())
}

pub fn insert_trace(value: u128) -> Result<usize, AtomicSetError> {
    let non_zero = NonZeroU128::new(value).ok_or_else(|| {
        AtomicSetError::InvalidValue("A span with id 0 is not allowed".to_string())
    })?;
    ACTIVE_TRACES.insert(non_zero)
}

pub fn remove_trace(_value: u128, idx: usize) -> Result<(), AtomicSetError> {
    ACTIVE_TRACES.remove(idx)
}
