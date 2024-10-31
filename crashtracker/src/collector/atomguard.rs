// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
pub struct AtomGuardError;

impl fmt::Display for AtomGuardError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Reentrancy guard error: already in use")
    }
}

pub struct AtomGuard<'a> {
    counter: &'a AtomicU64,
}

// Needed for anyhow
impl Error for AtomGuardError {}

impl<'a> AtomGuard<'a> {
    pub fn new(counter: &'a AtomicU64) -> Result<Self, AtomGuardError> {
        // This uses a CAS to try and "take" the counter from 0 to 1.  This is better than doing an
        // increment, checking the old value, and then decrementing if it was 0, since this is
        // atomic.
        // This could be a bool, but keeping it an int.
        let result = counter.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst);

        match result {
            Ok(_) => Ok(AtomGuard { counter }),
            Err(_) => Err(AtomGuardError),
        }
    }
}

impl Drop for AtomGuard<'_> {
    fn drop(&mut self) {
        // Decrement the counter when the guard is dropped
        // If the CAS did what it said it would do, then in reality we could probably just set it
        // to 0, but let's decrement.
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}
