// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_capabilities_impl::{EnvCapability, NativeCapabilities, NativeEnvCapability};
use libdd_common::environment;

// Isolate the process-wide snapshot from the crate's other tests.
#[test]
fn native_capabilities_borrow_the_explicit_startup_snapshot() {
    let env = NativeEnvCapability::new();
    let bundle = NativeCapabilities::new();
    assert!(!environment::is_initialized());
    assert_eq!(env.get("PATH").unwrap(), None);
    assert_eq!(bundle.get("PATH").unwrap(), None);

    environment::initialize_from([("SERVICE", "startup"), ("EMPTY", "")]).unwrap();
    let value = env.get("SERVICE").unwrap().unwrap();
    assert_eq!(value, "startup");
    assert_eq!(bundle.get("SERVICE").unwrap(), Some(value));
    assert_eq!(env.get("EMPTY").unwrap(), Some(""));
    assert_eq!(bundle.get("PATH").unwrap(), None);
    assert_eq!(
        value.as_ptr(),
        environment::var_os_ref("SERVICE")
            .unwrap()
            .as_encoded_bytes()
            .as_ptr()
    );
}
