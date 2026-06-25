// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Smoke test for the FFI install/restore roundtrip.
//!
//! Installing GOT overrides mutates global process state, so this lives
//! as an integration test in its own binary rather than a unit test.
//! Miri can't execute the underlying dl_iterate_phdr/mprotect calls.

#![cfg(all(target_os = "linux", not(miri)))]

use libdd_common_ffi::VoidResult;
use libdd_heap_gotter_ffi::{
    ddog_heap_gotter_install, ddog_heap_gotter_is_installed, ddog_heap_gotter_restore,
};

fn assert_ok(result: VoidResult, what: &str) {
    match result {
        VoidResult::Ok => {}
        VoidResult::Err(err) => panic!("{what} failed: {err}"),
    }
}

#[test]
fn install_restore_roundtrip() {
    assert!(
        !ddog_heap_gotter_is_installed(),
        "expected clean process state before install"
    );

    assert_ok(ddog_heap_gotter_install(), "ddog_heap_gotter_install");
    assert!(
        ddog_heap_gotter_is_installed(),
        "is_installed should be true after install"
    );

    // Touch the heap while installed so the patched GOT actually gets
    // used. We just need the process to still be alive after this.
    let v: Vec<u8> = vec![0; 128];
    assert_eq!(v.len(), 128);
    drop(v);

    assert_ok(ddog_heap_gotter_restore(), "ddog_heap_gotter_restore");
    assert!(
        !ddog_heap_gotter_is_installed(),
        "is_installed should be false after restore"
    );
}
