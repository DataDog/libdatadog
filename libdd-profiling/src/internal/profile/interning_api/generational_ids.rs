// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::{AtomicU64, Ordering};

/// Opaque identifier for the profiler generation
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Generation {
    id: u64,
}

impl Generation {
    const IMMORTAL: Self = Self { id: u64::MAX };

    /// The only way to create a generation.  Guaranteed to give a new value each time.
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self {
            id: COUNTER.fetch_add(1, Ordering::Relaxed),
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

#[cold]
#[inline(never)]
fn generation_mismatch() -> anyhow::Error {
    anyhow::anyhow!("Condition failed: `self.generation == expected_generation || self.generation == Generation::IMMORTAL`")
}

impl<T: Copy> GenerationalId<T> {
    #[inline]
    pub fn get(&self, expected_generation: Generation) -> anyhow::Result<T> {
        if self.generation == expected_generation || self.generation == Generation::IMMORTAL {
            Ok(self.id)
        } else {
            Err(generation_mismatch())
        }
    }

    pub const fn new(id: T, generation: Generation) -> Self {
        Self { id, generation }
    }

    pub const fn new_immortal(id: T) -> Self {
        Self {
            id,
            generation: Generation::IMMORTAL,
        }
    }
}
