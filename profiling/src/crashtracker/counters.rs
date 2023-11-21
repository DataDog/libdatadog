// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::constants::*;
use std::{
    io::Write,
    sync::atomic::{AtomicIsize, Ordering::SeqCst},
};

// TODO, add more as needed
/// This is a list of possible operations a profiler might be in, to help us
/// know
/// 1. Whether the profiler was running when the crash happened
/// 2. What it was doing at a broad level
/// This could also be used to track wall clock time, if that's not too expensive
#[repr(C)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ProfilingOpTypes {
    NotProfiling = 0,
    CollectingSample,
    Unwinding,
    Serializing,
    SIZE,
}

impl ProfilingOpTypes {
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

#[allow(clippy::declare_interior_mutable_const)]
const ATOMIC_ZERO: AtomicIsize = AtomicIsize::new(0);

static NUM_THREADS_DOING_PROFILING: AtomicIsize = ATOMIC_ZERO;
static PROFILING_OP_COUNTERS: [AtomicIsize; ProfilingOpTypes::SIZE as usize] =
    [ATOMIC_ZERO; ProfilingOpTypes::SIZE as usize];

pub fn begin_profiling_op(op: ProfilingOpTypes) -> anyhow::Result<()> {
    if op != ProfilingOpTypes::NotProfiling {
        NUM_THREADS_DOING_PROFILING.fetch_add(1, SeqCst);
    }
    PROFILING_OP_COUNTERS[op as usize].fetch_add(1, SeqCst);
    // this can technically wrap around, but if we hit 2^63 ops we're doing
    // something else wrong.
    Ok(())
}

// TODO: I'm making everything SeqCst for now.  Could gain some performance by
// using a weaker ordering.
pub fn end_profiling_op(op: ProfilingOpTypes) -> anyhow::Result<()> {
    if op != ProfilingOpTypes::NotProfiling {
        let old = NUM_THREADS_DOING_PROFILING.fetch_sub(1, SeqCst);
        anyhow::ensure!(
            old > 0,
            "attempted to end profiling op '{op:?}' while global count was 0"
        );
    }
    let old = PROFILING_OP_COUNTERS[op as usize].fetch_sub(1, SeqCst);
    anyhow::ensure!(
        old > 0,
        "attempted to end profiling op '{op:?}' while op count was 0"
    );
    Ok(())
}

pub fn emit_counters(w: &mut impl Write) -> anyhow::Result<()> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_COUNTERS}")?;
    writeln!(
        w,
        "\"num_threads_doing_profiling\": {}",
        NUM_THREADS_DOING_PROFILING.load(SeqCst)
    )?;

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
/// Safety: This is NOT ATOMIC.  Should only be used when no conflicting updates
/// can occur, e.g. after a fork but before profiling ops start on the child.
pub fn reset_counters() -> anyhow::Result<()> {
    NUM_THREADS_DOING_PROFILING.store(0, SeqCst);
    for c in PROFILING_OP_COUNTERS.iter() {
        c.store(0, SeqCst);
    }
    Ok(())
}
