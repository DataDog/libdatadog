// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[rustfmt::skip]
include!("_includes.rs");
mod deserializers;
mod serde;

#[cfg(test)]
mod pb_test;
