// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::fetch::FileStorage;
use crate::file_change_tracker::{FilePath, UpdatedFiles};
use crate::{RemoteConfigData, RemoteConfigPath};
use std::ops::Deref;
use std::sync::{Arc, Mutex, MutexGuard};

/// A trivial local storage for remote config files.
pub struct RawFileStorage<P: ParseFile> {
    updated: Mutex<Vec<(Arc<RawFile<P>>, P)>>,
}

impl<P: ParseFile> Default for RawFileStorage<P> {
    fn default() -> Self {
        RawFileStorage {
            updated: Mutex::default(),
        }
    }
}

pub trait ParseFile
where
    Self: Sized,
{
    fn parse(path: &RemoteConfigPath, contents: Vec<u8>) -> Self;
}

impl<P: ParseFile> UpdatedFiles<RawFile<P>, P> for RawFileStorage<P> {
    fn updated(&self) -> Vec<(Arc<RawFile<P>>, P)> {
        std::mem::take(&mut *self.updated.lock().unwrap())
    }
}

/// Mutable data: version and contents.
struct RawFileData<P> {
    version: u64,
    contents: P,
}

/// File contents and file metadata
pub struct RawFile<P> {
    path: Arc<RemoteConfigPath>,
    data: Mutex<RawFileData<P>>,
}

pub struct RawFileContentsGuard<'a, P>(MutexGuard<'a, RawFileData<P>>);

impl<P> Deref for RawFileContentsGuard<'_, P> {
    type Target = P;

    fn deref(&self) -> &Self::Target {
        &self.0.contents
    }
}

impl<P> RawFile<P> {
    /// Gets the contents behind a Deref impl (guarding a Mutex).
    pub fn contents(&self) -> RawFileContentsGuard<P> {
        RawFileContentsGuard(self.data.lock().unwrap())
    }

    pub fn version(&self) -> u64 {
        self.data.lock().unwrap().version
    }
}

impl<P> FilePath for RawFile<P> {
    fn path(&self) -> &RemoteConfigPath {
        &self.path
    }
}

impl<P: ParseFile> FileStorage for RawFileStorage<P> {
    type StoredFile = RawFile<P>;

    fn store(
        &self,
        version: u64,
        path: Arc<RemoteConfigPath>,
        contents: Vec<u8>,
    ) -> anyhow::Result<Arc<Self::StoredFile>> {
        Ok(Arc::new(RawFile {
            data: Mutex::new(RawFileData {
                version,
                contents: P::parse(&path, contents),
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
        let mut contents = P::parse(&file.path, contents);
        let mut data = file.data.lock().unwrap();
        std::mem::swap(&mut data.contents, &mut contents);
        self.updated.lock().unwrap().push((file.clone(), contents));
        data.version = version;
        Ok(())
    }
}

/// It simply stores the raw remote config file contents.
pub type SimpleFileStorage = RawFileStorage<Vec<u8>>;

impl ParseFile for Vec<u8> {
    fn parse(_path: &RemoteConfigPath, contents: Vec<u8>) -> Self {
        contents
    }
}

/// Storing the remote config file contents in parsed form
pub type ParsedFileStorage = RawFileStorage<anyhow::Result<RemoteConfigData>>;

impl ParseFile for anyhow::Result<RemoteConfigData> {
    fn parse(path: &RemoteConfigPath, contents: Vec<u8>) -> Self {
        RemoteConfigData::try_parse(path.product, contents.as_slice())
    }
}
