// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Portable capability traits for cross-platform libdatadog.

pub mod env;
pub mod file;
pub mod http;
pub mod log_output;
pub mod maybe_send;
pub mod sleep;
pub mod spawn;

pub use self::env::{EnvCapability, EnvError};
pub use self::file::{FileCapability, FileError, FileMetadata};
pub use self::http::{
    BodySender, BufferingBodySender, ChunkFuture, HttpClientCapability, HttpError, ResponseFuture,
    StreamingBodySender,
};
pub use self::log_output::LogWriterCapability;
pub use self::sleep::SleepCapability;
pub use self::spawn::SpawnError;
pub use ::http::{Request, Response};
pub use bytes::Bytes;
pub use maybe_send::{MaybeSend, MaybeSendFuture};
