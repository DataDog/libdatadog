// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(feature = "std", unix))]

use libdd_common::environment::{self, InitError, VarError};

// A separate integration-test process keeps the process-wide snapshot out of other tests.
#[test]
fn origin_discovery_keeps_startup_values_after_c_environment_changes() {
    assert!(!environment::is_initialized());
    // SAFETY: this isolated test has no concurrent environment writers.
    unsafe {
        assert_eq!(
            libc::setenv(c"DD_EXTERNAL_ENV".as_ptr(), c"startup-value".as_ptr(), 1),
            0
        );
        assert_eq!(libc::unsetenv(c"LIBDD_SHADOW_LATE_VALUE".as_ptr()), 0);
    }
    assert_eq!(
        environment::var("DD_EXTERNAL_ENV").unwrap(),
        "startup-value"
    );

    // SAFETY: no other code in this test process mutates the environment during the copy.
    unsafe { environment::initialize() }.unwrap();

    // SAFETY: all reads below use owned shadow storage; there are no process-env readers.
    unsafe {
        assert_eq!(
            libc::setenv(c"DD_EXTERNAL_ENV".as_ptr(), c"changed-value".as_ptr(), 1),
            0
        );
        assert_eq!(
            libc::setenv(
                c"LIBDD_SHADOW_LATE_VALUE".as_ptr(),
                c"late-value".as_ptr(),
                1
            ),
            0
        );
    }
    assert_eq!(
        environment::var("DD_EXTERNAL_ENV").unwrap(),
        "startup-value"
    );
    assert_eq!(
        environment::var("LIBDD_SHADOW_LATE_VALUE"),
        Err(VarError::NotPresent)
    );
    assert_eq!(
        environment::initialize_from([("DD_EXTERNAL_ENV", "replacement")]),
        Err(InitError::AlreadyInitialized)
    );
    assert_eq!(
        libdd_common::entity_id::get_entity_headers()
            .find(|(name, _)| *name == "datadog-external-env")
            .map(|(_, value)| value),
        Some("startup-value")
    );

    // Miri does not support rustix's stat64 call; the snapshot assertions above still run.
    #[cfg(all(target_os = "linux", not(miri)))]
    assert_eq!(
        libdd_common::entity_id::discover().external_env.as_deref(),
        Some("startup-value")
    );
}
