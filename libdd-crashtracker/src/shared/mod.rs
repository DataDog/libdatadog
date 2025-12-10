// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module holds constants/structures that are shared between the collector and receiver

pub(crate) mod configuration;

#[cfg(not(feature = "benchmarking"))]
pub(crate) mod constants;

pub(crate) mod log;

#[cfg(feature = "benchmarking")]
pub mod constants;
