// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Portable capability traits for cross-platform libdatadog.

pub mod http;
pub mod maybe_send;
pub mod sleep;
pub mod spawn;

pub use self::http::{HttpClientCapability, HttpError};
pub use self::sleep::SleepCapability;
pub use self::spawn::{SpawnCapability, SpawnError};
pub use ::http::{Request, Response};
pub use bytes::Bytes;
pub use maybe_send::MaybeSend;
