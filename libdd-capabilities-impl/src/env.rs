// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native environment-variable capability backed by the shared immutable startup snapshot.
//!
//! Initialize explicitly through `libdd_common::environment` before reading. Constructing
//! the capability does not capture the environment; reads before initialization return absence.

pub use libdd_common::environment::ShadowEnvCapability as NativeEnvCapability;

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_capabilities::EnvCapability;

    #[test]
    fn get_absent_returns_none() {
        let cap = NativeEnvCapability;
        // A name unlikely to be set in any environment.
        assert_eq!(cap.get("LIBDD_CAP_TEST_ABSENT_XYZZY").unwrap(), None);
    }
}
