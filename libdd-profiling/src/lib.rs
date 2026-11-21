// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![no_std]

// The `alloc` crate backs `collections::alloc::Global`
#[cfg(any(test, feature = "alloc"))]
extern crate alloc as rust_alloc;
// Unit tests use std's test harness and panics.
#[cfg(test)]
extern crate std;

pub mod collections;
#[cfg(any(test, feature = "alloc"))]
pub mod compress;
#[cfg(any(test, feature = "alloc"))]
pub mod encode;
#[cfg(test)]
#[cfg(all(feature = "pprof", feature = "otel"))]
mod end_to_end;
pub mod intern_table;
#[cfg(any(test, feature = "alloc"))]
pub mod profile;
#[cfg(any(test, feature = "alloc"))]
pub mod request;
