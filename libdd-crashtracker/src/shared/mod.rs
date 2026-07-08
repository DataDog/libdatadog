// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module holds constants/structures that are shared between the collector and receiver

pub(crate) mod defaults;
pub(crate) mod signal_names;
pub(crate) mod signals;
pub(crate) mod stacktrace_collection;
pub(crate) mod tag_keys;
pub(crate) mod ucontext;

#[cfg(feature = "std")]
pub(crate) mod configuration;

#[cfg(all(feature = "std", not(feature = "benchmarking")))]
pub(crate) mod constants;

#[cfg(all(feature = "std", feature = "benchmarking"))]
pub mod constants;
