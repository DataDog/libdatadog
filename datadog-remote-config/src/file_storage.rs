// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::fetch::FileStorage;
use crate::file_change_tracker::{FilePath, UpdatedFiles};
use crate::RemoteConfigPath;
use libdd_common::MutexExt;
use std::ops::Deref;
use std::sync::{Arc, Mutex, MutexGuard};

/// A trivial local storage for remote config files.
pub struct RawFileStorage<P: ParseFile> {
    parser: P,
    #[allow(clippy::type_complexity)]
    updated: Mutex<Vec<(Arc<RawFile<P::Parsed>>, P::Parsed)>>,
}

impl<P: ParseFile + Default> Default for RawFileStorage<P> {
    fn default() -> Self {
        Self::new(P::default())
    }
}

impl<P: ParseFile> RawFileStorage<P> {
    pub fn new(parser: P) -> Self {
        RawFileStorage {
            parser,
            updated: Mutex::default(),
        }
    }
}

/// Instance-based file parser. Implementations may carry state (e.g. configuration to drive a
/// product-specific parsing decision).
pub trait ParseFile: Clone + Send + Sync {
    /// The type of the parsed/stored content.
    type Parsed: Send;

    fn parse(&self, path: &RemoteConfigPath, contents: Vec<u8>) -> Self::Parsed;
}

impl<P: ParseFile> UpdatedFiles<RawFile<P::Parsed>, P::Parsed> for RawFileStorage<P> {
    fn updated(&self) -> Vec<(Arc<RawFile<P::Parsed>>, P::Parsed)> {
        std::mem::take(&mut *self.updated.lock_or_panic())
    }
}

/// Mutable data: version and contents.
struct RawFileData<T> {
    version: u64,
    contents: T,
}

/// File contents and file metadata
pub struct RawFile<T> {
    path: Arc<RemoteConfigPath>,
    data: Mutex<RawFileData<T>>,
}

pub struct RawFileContentsGuard<'a, T>(MutexGuard<'a, RawFileData<T>>);

impl<T> Deref for RawFileContentsGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.contents
    }
}

impl<T> RawFile<T> {
    /// Gets the contents behind a Deref impl (guarding a Mutex).
    pub fn contents(&self) -> RawFileContentsGuard<'_, T> {
        RawFileContentsGuard(self.data.lock_or_panic())
    }

    pub fn version(&self) -> u64 {
        self.data.lock_or_panic().version
    }
}

impl<T> FilePath for RawFile<T> {
    fn path(&self) -> &RemoteConfigPath {
        &self.path
    }
}

impl<P: ParseFile> FileStorage for RawFileStorage<P> {
    type StoredFile = RawFile<P::Parsed>;

    fn store(
        &self,
        version: u64,
        path: Arc<RemoteConfigPath>,
        contents: Vec<u8>,
    ) -> anyhow::Result<Arc<Self::StoredFile>> {
        Ok(Arc::new(RawFile {
            data: Mutex::new(RawFileData {
                version,
                contents: self.parser.parse(&path, contents),
            }),
            path,
        }))
    }

    fn update(
        &self,
        file: &Arc<Self::StoredFile>,
        version: u64,
        contents: Vec<u8>,
    ) -> anyhow::Result<()> {
        let mut contents = self.parser.parse(&file.path, contents);
        let mut data = file.data.lock_or_panic();
        std::mem::swap(&mut data.contents, &mut contents);
        self.updated.lock_or_panic().push((file.clone(), contents));
        data.version = version;
        Ok(())
    }
}

// ── RawBytesParser ────────────────────────────────────────────────────────────

/// Stores raw remote config file bytes without parsing.
#[derive(Clone, Default)]
pub struct RawBytesParser;

impl ParseFile for RawBytesParser {
    type Parsed = Vec<u8>;

    fn parse(&self, _path: &RemoteConfigPath, contents: Vec<u8>) -> Vec<u8> {
        contents
    }
}

/// Stores the remote config file contents in raw (unparsed) form.
pub type SimpleFileStorage = RawFileStorage<RawBytesParser>;

/// Stores remote config file contents parsed as [`crate::BuiltinProducts`].
pub type ParsedFileStorage = RawFileStorage<crate::parse::BuiltinProductsParser>;
