// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::AtomicU64;

/// Opaque identifier for the profiler generation
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Generation {
    id: u64,
}

impl Generation {
    /// The only way to create a generation.  Guaranteed to give a new value each time.
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self {
            id: COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        }
    }
}
impl Default for Generation {
    fn default() -> Self {
        Self::new()
    }
}

#[repr(C)]
pub struct GenerationalId<T: Copy> {
    generation: Generation,
    id: T,
}

impl<T: Copy> GenerationalId<T> {
    pub fn get(&self, expected_generation: Generation) -> anyhow::Result<T> {
        anyhow::ensure!(self.generation == expected_generation);
        Ok(self.id)
    }

    pub fn new(id: T, generation: Generation) -> Self {
        Self { id, generation }
    }
}
