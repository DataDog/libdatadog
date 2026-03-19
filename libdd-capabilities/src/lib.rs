// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Portable capability traits for cross-platform libdatadog.

pub mod http;
pub mod maybe_send;

pub use self::http::{HttpClientTrait, HttpError};
pub use ::http::{Request, Response};
pub use bytes::Bytes;
pub use maybe_send::MaybeSend;
