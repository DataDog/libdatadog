// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::fetch::FileStorage;
use crate::file_change_tracker::FilePath;
use crate::parse::{ParserRegistry, default_registry};
use crate::{RemoteConfigParsed, RemoteConfigPath};
use std::ops::Deref;
use std::sync::Arc;

/// A trivial local storage for remote config files.
pub struct RawFileStorage<P: ParseFile> {
    parser: P,
}

impl<P: ParseFile + Default> Default for RawFileStorage<P> {
    fn default() -> Self {
        Self::new(P::default())
    }
}

impl<P: ParseFile> RawFileStorage<P> {
    pub fn new(parser: P) -> Self {
        RawFileStorage { parser }
    }
}

/// Instance-based file parser. Implementations may carry state (e.g. an [`Arc<ParserRegistry>`]).
pub trait ParseFile: Clone + Send + Sync {
    /// The type of the parsed/stored content.
    type Parsed: Send;

    fn parse(&self, path: &RemoteConfigPath, contents: Vec<u8>) -> Self::Parsed;
}

/// File contents and file metadata. Immutable: a new version of a file is a new `RawFile`.
pub struct RawFile<T> {
    path: Arc<RemoteConfigPath>,
    version: u64,
    contents: T,
}

pub struct RawFileContentsGuard<'a, T>(&'a T);

impl<T> Deref for RawFileContentsGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl<T> RawFile<T> {
    /// Gets the contents behind a Deref impl.
    pub fn contents(&self) -> RawFileContentsGuard<'_, T> {
        RawFileContentsGuard(&self.contents)
    }

    pub fn version(&self) -> u64 {
        self.version
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
            version,
            contents: self.parser.parse(&path, contents),
            path,
        }))
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

// ── RegistryParser ────────────────────────────────────────────────────────────

/// A [`ParseFile`] implementation that dispatches to a [`ParserRegistry`].
#[derive(Clone)]
pub struct RegistryParser(pub Arc<ParserRegistry>);

impl ParseFile for RegistryParser {
    type Parsed = anyhow::Result<Option<RemoteConfigParsed>>;

    fn parse(&self, path: &RemoteConfigPath, contents: Vec<u8>) -> Self::Parsed {
        self.0.parse(path.product(), &contents)
    }
}

/// Stores the remote config file contents in parsed form using a [`ParserRegistry`].
pub type ParsedFileStorage = RawFileStorage<RegistryParser>;

impl ParsedFileStorage {
    /// Create a storage backed by the given registry.
    pub fn with_registry(registry: Arc<ParserRegistry>) -> Self {
        RawFileStorage::new(RegistryParser(registry))
    }
}

impl Default for ParsedFileStorage {
    fn default() -> Self {
        Self::with_registry(Arc::new(default_registry()))
    }
}
