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
// The CXX module is crate-private. Its types are intended for C++ callers
// via the generated bridge, not for downstream Rust crates. Exposing them
// publicly would let safe Rust code reach unsound APIs (e.g. dictionary
// sample functions that rely on C++-side lifetime invariants).
#[cfg(feature = "cxx")]
pub(crate) mod cxx;
pub mod exporter;
pub mod internal;
pub mod iter;
pub mod pprof;
pub mod profiles;
