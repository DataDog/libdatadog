// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! File-system capability trait and error types.
//!
//! Async so a wasm impl can await `fs.promises`. Paths are `&str` because
//! wasm callers hand them across the JS boundary as strings.

use crate::maybe_send::MaybeSend;
use core::future::Future;

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("File not found: {0}")]
    NotFound(String),
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    #[error("IO error: {0}")]
    Io(anyhow::Error),
}

/// Snapshot of a file-system entry's metadata.
///
/// `inode` is `None` when the underlying platform does not expose one (Windows
/// via `std`). Node.js exposes an inode on every platform, so the wasm impl
/// always populates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMetadata {
    pub size: u64,
    pub inode: Option<u64>,
    pub is_file: bool,
    pub is_dir: bool,
}

pub trait FileCapability: Clone + std::fmt::Debug {
    fn new() -> Self;

    fn read(&self, path: &str)
        -> impl Future<Output = Result<bytes::Bytes, FileError>> + MaybeSend;

    fn write(
        &self,
        path: &str,
        contents: bytes::Bytes,
    ) -> impl Future<Output = Result<(), FileError>> + MaybeSend;

    fn metadata(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<FileMetadata, FileError>> + MaybeSend;

    fn exists(&self, path: &str) -> impl Future<Output = Result<bool, FileError>> + MaybeSend;
}
