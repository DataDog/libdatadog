// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(all(test, not(miri)))]
pub trait TestInstance {
    fn test_instance(seed: u64) -> Self;
}
