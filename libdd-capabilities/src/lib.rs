// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Portable capability traits for cross-platform libdatadog.
//!
//! Without default features, only the allocation-free environment capability is available.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod env;
#[cfg(feature = "std")]
pub mod file;
#[cfg(feature = "std")]
pub mod http;
#[cfg(feature = "std")]
pub mod log_output;
#[cfg(feature = "std")]
pub mod maybe_send;
#[cfg(feature = "std")]
pub mod sleep;
#[cfg(feature = "std")]
pub mod spawn;

pub use self::env::{EnvCapability, EnvError};
#[cfg(feature = "std")]
pub use self::file::{FileCapability, FileError, FileMetadata};
#[cfg(feature = "std")]
pub use self::http::{
    BodySender, BufferingBodySender, ChunkFuture, HttpClientCapability, HttpError, ResponseFuture,
    StreamingBodySender,
};
#[cfg(feature = "std")]
pub use self::log_output::LogWriterCapability;
#[cfg(feature = "std")]
pub use self::sleep::SleepCapability;
#[cfg(feature = "std")]
pub use self::spawn::SpawnError;
#[cfg(feature = "std")]
pub use ::http::{Request, Response};
#[cfg(feature = "std")]
pub use bytes::Bytes;
#[cfg(feature = "std")]
pub use maybe_send::{MaybeSend, MaybeSendFuture};
