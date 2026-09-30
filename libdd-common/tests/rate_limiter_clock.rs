// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(not(target_arch = "wasm32"))]

use core::time::Duration;
use libdd_common::rate_limiter::now;
use std::time::Instant;

#[test]
#[cfg_attr(miri, ignore)]
fn clock_uses_nanoseconds() {
    let started = Instant::now();
    let before = now();
    std::thread::sleep(Duration::from_millis(20));
    let after = now();
    let elapsed = started.elapsed();
    let measured = Duration::from_nanos(after.checked_sub(before).expect("clock moved backwards"));

    assert!(
        measured >= Duration::from_millis(10),
        "clock advanced {measured:?}"
    );
    assert!(
        measured <= elapsed + Duration::from_millis(1),
        "clock advanced {measured:?} in {elapsed:?}"
    );
}
