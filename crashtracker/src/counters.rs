// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::constants::*;
use std::{
    io::Write,
    sync::atomic::{AtomicI64, Ordering::SeqCst},
};

/// This enum represents operations a profiler might be engaged in.
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
pub enum ProfilingOpTypes {
    // TODO: Do we want this, or just keep it implicit?
    NotProfiling = 0,
    CollectingSample,
    Unwinding,
    Serializing,
    /// Dummy value to allow easier iteration
    SIZE,
}

impl ProfilingOpTypes {
    /// A static string giving the name of the `ProfilingOpType`.
    /// We implement this, rather than `to_string`, to avoid the memory
    /// allocation associated with `String`.
    pub fn name(i: usize) -> anyhow::Result<&'static str> {
        let rval = match i {
            0 => "not_profiling",
            1 => "collecting_sample",
            2 => "unwinding",
            3 => "serializing",
            _ => anyhow::bail!("invalid enum val {i}"),
        };
        Ok(rval)
    }
}

// In this case, we actually WANT multiple copies of the interior mutable struct
#[allow(clippy::declare_interior_mutable_const)]
const ATOMIC_ZERO: AtomicI64 = AtomicI64::new(0);

// TODO: Is this
static PROFILING_OP_COUNTERS: [AtomicI64; ProfilingOpTypes::SIZE as usize] =
    [ATOMIC_ZERO; ProfilingOpTypes::SIZE as usize];

/// Track that a profiling operation (of type op) has begun.
/// Currently, we assume states are discrete (i.e. not nested).
/// PRECONDITIONS:
///     This function assumes that the crash-tracker is initialized.
/// ATOMICITY:
///     This function is atomic.  
pub fn begin_profiling_op(op: ProfilingOpTypes) -> anyhow::Result<()> {
    // TODO: I'm making everything SeqCst for now.  Could possibly gain some
    // performance by using a weaker ordering.
    let old = PROFILING_OP_COUNTERS[op as usize].fetch_add(1, SeqCst);
    anyhow::ensure!(old < i64::MAX, "Overflowed counter {op:?}");
    Ok(())
}

/// Track that a profiling operation (of type op) has finished.
/// Currently, we assume states are discrete (i.e. not nested).
/// PRECONDITIONS: This function assumes that the crash-tracker is initialized.
/// ATOMICITY: This function is atomic.  
pub fn end_profiling_op(op: ProfilingOpTypes) -> anyhow::Result<()> {
    let old = PROFILING_OP_COUNTERS[op as usize].fetch_sub(1, SeqCst);
    anyhow::ensure!(old > 0, "Can't end profiling '{op:?}' with count 0");
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
pub fn emit_counters(w: &mut impl Write) -> anyhow::Result<()> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_COUNTERS}")?;
    for (i, c) in PROFILING_OP_COUNTERS.iter().enumerate() {
        writeln!(
            w,
            "{{\"{}\": {}}}",
            ProfilingOpTypes::name(i)?,
            c.load(SeqCst)
        )?;
    }
    writeln!(w, "{DD_CRASHTRACK_END_COUNTERS}")?;
    Ok(())
}

/// Resets all counters to 0.
/// Expected to be used after a fork, to reset the counters on the child
/// ATOMICITY:
///     This is NOT ATOMIC.
///     Should only be used when no conflicting updates can occur,
///     e.g. after a fork but before profiling ops start on the child.
pub fn reset_counters() -> anyhow::Result<()> {
    for c in PROFILING_OP_COUNTERS.iter() {
        c.store(0, SeqCst);
    }
    Ok(())
}
