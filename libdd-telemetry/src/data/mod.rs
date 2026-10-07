// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "alloc")]
mod common;
#[cfg(feature = "std")]
mod payloads;

#[cfg(feature = "alloc")]
pub use common::*;
#[cfg(feature = "std")]
pub use payload::*;
#[cfg(feature = "std")]
pub use payloads::*;
pub mod metrics;
#[cfg(feature = "std")]
pub mod payload;
