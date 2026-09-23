// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod api;
pub mod api2;
pub mod collections;
// The CXX module's types are intended for C++ callers via the generated bridge,
// not for downstream Rust crates. #[doc(hidden)] keeps them out of rustdoc.
// The module must be pub because the dd-trace-py native crate re-exports it
// for the C++ build system to find the generated CXX bridge headers.
#[cfg(feature = "cxx")]
#[doc(hidden)]
pub mod cxx;
pub mod exporter;
pub mod internal;
#[cfg(feature = "otel")]
pub mod otel;
pub mod iter;
pub mod pprof;
pub mod profiles;
