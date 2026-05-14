// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Rust types for Datadog's `agent-payload` healthplatform protobuf schema.
//!
//! The crate is `no_std + alloc`. Encode/decode are provided by the prost
//! `Message` trait, which every generated type already implements — bring
//! `prost::Message` into scope and call `encode_to_vec()` / `decode(bytes)`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

extern crate alloc;

mod healthplatform {
    include!("healthplatform.rs");
}

pub use healthplatform::*;
