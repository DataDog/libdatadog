// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum SignalOwner {
    #[cfg(feature = "collector")]
    StdCollector = 1,
    #[cfg(feature = "collector_signal-safe")]
    SignalSafeCollector = 2,
}

static OWNER: AtomicU8 = AtomicU8::new(0);

pub(crate) fn acquire(owner: SignalOwner) -> bool {
    let owner = owner as u8;
    OWNER
        .compare_exchange(0, owner, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
        || OWNER.load(Ordering::Acquire) == owner
}

pub(crate) fn release(owner: SignalOwner) {
    let _ = OWNER.compare_exchange(owner as u8, 0, Ordering::AcqRel, Ordering::Acquire);
}
