// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[rustfmt::skip]
pub mod pb;
pub mod remoteconfig;
mod serde;

#[cfg(test)]
mod pb_test;
