// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Profiling arena used by the string table and the profile sets.
//!
//! Without `allocation-chaos-test` this is `ChainAllocator<VirtualAllocator>`. With that
//! test-only feature, logical requests and backing-chunk growth are wrapped so the
//! allocation-failure harness can fail either one.

use libdd_alloc::{ChainAllocator, VirtualAllocator};

#[cfg(not(feature = "allocation-chaos-test"))]
pub(crate) type ProfilingArena = ChainAllocator<VirtualAllocator>;

#[cfg(feature = "allocation-chaos-test")]
pub(crate) type ProfilingArena = libdd_alloc::ChaosAllocator<
    ChainAllocator<libdd_alloc::ChaosAllocator<VirtualAllocator, libdd_alloc::ArenaBacking>>,
    libdd_alloc::ArenaRequest,
>;

pub(crate) fn profiling_arena(chunk_size_hint: usize) -> ProfilingArena {
    #[cfg(not(feature = "allocation-chaos-test"))]
    {
        ChainAllocator::new_in(chunk_size_hint, VirtualAllocator {})
    }
    #[cfg(feature = "allocation-chaos-test")]
    {
        let backing = libdd_alloc::ChaosAllocator::new(VirtualAllocator {});
        libdd_alloc::ChaosAllocator::new(ChainAllocator::new_in(chunk_size_hint, backing))
    }
}
