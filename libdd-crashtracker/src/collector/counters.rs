// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::{AtomicI64, Ordering::SeqCst};
use thiserror::Error;

#[cfg(unix)]
use std::io::Write;

/// This enum represents operations a the tracked library might be engaged in.
/// Currently only implemented for profiling.
/// The idea is that if a crash consistently occurs while a particular operation
/// is ongoing, its likely related.
///
/// In the future, we might also track wall-clock time of operations
/// (or some statistical sampling thereof) using the same enum.
///
/// NOTE: This enum is known to be non-exhaustive.  Feel free to add new types
///       as needed.
#[repr(C)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum OpTypes {
    ProfilerInactive = 0,
    ProfilerCollectingSample,
    ProfilerUnwinding,
    ProfilerSerializing,
    /// Dummy value to allow easier iteration
    SIZE,
}

impl OpTypes {
    /// A static string giving the name of the `ProfilingOpType`.
    /// We implement this, rather than `to_string`, to avoid the memory
    /// allocation associated with `String`.
    pub fn name(i: usize) -> Result<&'static str, CounterError> {
        let rval = match i {
            0 => "profiler_inactive",
            1 => "profiler_collecting_sample",
            2 => "profiler_unwinding",
            3 => "profiler_serializing",
            _ => return Err(CounterError::InvalidEnumValue(i)),
        };
        Ok(rval)
    }
}

// In this case, we actually WANT multiple copies of the interior mutable struct
#[allow(clippy::declare_interior_mutable_const)]
const ATOMIC_ZERO: AtomicI64 = AtomicI64::new(0);

// TODO: Is this
static OP_COUNTERS: [AtomicI64; OpTypes::SIZE as usize] = [ATOMIC_ZERO; OpTypes::SIZE as usize];

/// Track that an operation (of type op) has begun.
/// Currently, we assume states are discrete (i.e. not nested).
/// PRECONDITIONS:
///     This function assumes that the crash-tracker is initialized.
/// ATOMICITY:
///     This function is atomic.
pub fn begin_op(op: OpTypes) -> Result<(), CounterError> {
    // TODO: I'm making everything SeqCst for now.  Could possibly gain some
    // performance by using a weaker ordering.
    let old = OP_COUNTERS[op as usize].fetch_add(1, SeqCst);
    if old == i64::MAX - 1 {
        return Err(CounterError::CounterOverflow(op));
    }
    Ok(())
}

/// Track that an operation (of type op) has finished.
/// Currently, we assume states are discrete (i.e. not nested).
/// PRECONDITIONS: This function assumes that the crash-tracker is initialized.
/// ATOMICITY: This function is atomic.  
pub fn end_op(op: OpTypes) -> Result<(), CounterError> {
    let old = OP_COUNTERS[op as usize].fetch_sub(1, SeqCst);
    if old <= 0 {
        return Err(CounterError::OperationNotStarted(op));
    }
    Ok(())
}

/// Emits the counters as structured json to the given writer.
/// In particular, a series of lines:
///
/// DD_CRASHTRACK_BEGIN_COUNTERS
/// {"counter_1_name": counter_1_value}
/// {"counter_2_name": counter_2_value}
/// ...
/// {"counter_n_name": counter_n_value}
/// DD_CRASHTRACK_END_COUNTERS
///
/// PRECONDITIONS:
///     This function assumes that the crash-tracker is initialized.
/// ATOMICITY:
///     This accesses to each counter is atomic.  However, iterating over the
///     array is not.
/// SIGNAL SAFETY:
///     This function is careful to only write to the handle, without doing any
///     unnecessary mutexes or memory allocation.
#[cfg(unix)]
pub fn emit_counters(w: &mut impl Write) -> Result<(), CounterError> {
    use crate::shared::constants::*;

    writeln!(w, "{DD_CRASHTRACK_BEGIN_COUNTERS}")?;
    for (i, c) in OP_COUNTERS.iter().enumerate() {
        writeln!(w, "{{\"{}\": {}}}", OpTypes::name(i)?, c.load(SeqCst))?;
    }
    writeln!(w, "{DD_CRASHTRACK_END_COUNTERS}")?;
    w.flush()?;
    Ok(())
}

/// Resets all counters to 0.
/// Expected to be used after a fork, to reset the counters on the child
/// ATOMICITY:
///     This is NOT ATOMIC.
///     Should only be used when no conflicting updates can occur,
///     e.g. after a fork but before ops start on the child.
pub fn reset_counters() -> Result<(), CounterError> {
    for c in OP_COUNTERS.iter() {
        c.store(0, SeqCst);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum CounterError {
    #[error("Invalid enum value: {0}")]
    InvalidEnumValue(usize),
    #[error("Counter overflow for operation {0:?}")]
    CounterOverflow(OpTypes),
    #[error("Attempted to end operation {0:?} but it was never started or already ended")]
    OperationNotStarted(OpTypes),
    #[error("Failed to write to output: {0}")]
    WriteError(#[from] std::io::Error),
}
