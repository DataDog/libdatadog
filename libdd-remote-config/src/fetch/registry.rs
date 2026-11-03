// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The file versions stored under one set of [`super::ConfigInvariants`], indexed by path and
//! hash. Several versions of a path can be alive at once, since each client keeps the version it
//! was last handed until its own next poll.
//!
//! This module is free of any async runtime, so that it builds wherever the fetcher does.

use crate::RemoteConfigPath;
use hashbrown::{HashMap, HashSet};
use libdd_common::MutexExt;
use libdd_trace_protobuf::remoteconfig::TargetFileMeta;
use std::sync::{Arc, Mutex, MutexGuard};

/// The stored file versions, by path. A version is identified by (path, hash, length), which is
/// what the agent deduplicates on: its version number plays no part in it. A version-only change
/// therefore maps to the same entry.
///
/// Storing a version never replaces another one. Which version of a path is advertised is
/// computed from the versions responses named each entry with, so a client being served an older
/// version cannot displace the newest one.
pub(crate) struct FileRegistry<F> {
    paths: Mutex<Versions<F>>,
}

type Versions<F> = HashMap<Arc<RemoteConfigPath>, Vec<VersionEntry<F>>>;

/// Exclusive access to the [`FileRegistry`], for operations which must be atomic with respect to
/// other fetchers sharing it.
pub(crate) struct RegistryGuard<'a, F>(MutexGuard<'a, Versions<F>>);

/// One stored version: one (path, hash, length).
pub(crate) struct VersionEntry<F> {
    /// The primary hash of the contents, as chosen by the caller (the agent protocol uses sha256,
    /// else sha512).
    hash: String,
    /// What is advertised for this version: path, length and all hashes.
    meta: TargetFileMeta,
    /// The highest version a response named this entry with.
    newest_version: u64,
    file: Arc<F>,
    /// No client holds this version anymore: it is no longer advertised, and is about to be
    /// removed. A response naming it again revives it.
    expiring: bool,
}

impl<F> Default for FileRegistry<F> {
    fn default() -> Self {
        FileRegistry {
            paths: Default::default(),
        }
    }
}

impl<F> FileRegistry<F> {
    /// Locks the registry for a sequence of operations.
    pub(crate) fn lock(&self) -> RegistryGuard<'_, F> {
        RegistryGuard(self.paths.lock_or_panic())
    }

    /// The versions to list in `cached_target_files`, see [`RegistryGuard::advertise`].
    pub(crate) fn advertise(&self) -> Vec<TargetFileMeta> {
        self.lock().advertise()
    }

    /// Removes every version. The files are released after the lock is.
    pub(crate) fn clear(&self) {
        let removed = std::mem::take(&mut *self.paths.lock_or_panic());
        drop(removed);
    }

    /// The number of paths with a stored version.
    pub(crate) fn paths(&self) -> u32 {
        self.paths.lock_or_panic().len() as u32
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.paths.lock_or_panic().is_empty()
    }

    /// Every stored version, sorted by path, then version.
    #[cfg(test)]
    pub(crate) fn entries(&self) -> Vec<EntryInfo<F>> {
        let mut entries: Vec<_> = self
            .paths
            .lock_or_panic()
            .iter()
            .flat_map(|(path, versions)| {
                versions.iter().map(|entry| EntryInfo {
                    path: path.to_string(),
                    hash: entry.hash.clone(),
                    version: entry.newest_version,
                    expiring: entry.expiring,
                    file: entry.file.clone(),
                })
            })
            .collect();
        entries.sort_by(|a, b| (&a.path, a.version).cmp(&(&b.path, b.version)));
        entries
    }
}

/// A stored version, as seen by tests.
#[cfg(test)]
pub(crate) struct EntryInfo<F> {
    pub path: String,
    pub hash: String,
    /// The highest version a response named the entry with.
    pub version: u64,
    pub expiring: bool,
    pub file: Arc<F>,
}

impl<F> RegistryGuard<'_, F> {
    /// The versions to list in `cached_target_files`: per path, the version most recently named
    /// by a response, of those not expiring. Older live versions are not listed: the agent only
    /// compares cached files with its current targets, which name the newest one.
    ///
    /// Empty files are not listed: the agent rejects a request listing a cached file whose length
    /// is below 1, and not listing them only makes it resend their empty body.
    pub(crate) fn advertise(&self) -> Vec<TargetFileMeta> {
        self.0
            .values()
            .filter_map(|versions| {
                versions
                    .iter()
                    .filter(|entry| !entry.expiring)
                    .max_by_key(|entry| entry.newest_version)
            })
            .filter(|entry| entry.meta.length > 0)
            .map(|entry| entry.meta.clone())
            .collect()
    }

    /// The stored version of `path` with the given primary hash and length, if any, along with
    /// the registry's copy of the path.
    pub(crate) fn lookup(
        &self,
        path: &RemoteConfigPath,
        hash: &str,
        len: u64,
    ) -> Option<(Arc<RemoteConfigPath>, Arc<F>)> {
        let (path, versions) = self.0.get_key_value(path)?;
        let entry = versions.iter().find(|entry| entry.matches(hash, len))?;
        Some((path.clone(), entry.file.clone()))
    }

    /// Adds a newly stored version, next to the other versions of its path. The length it is
    /// found under is `meta.length`. Returns the registry's copy of the path and the stored file:
    /// should the version already be stored, that one is kept, and `file` is dropped.
    pub(crate) fn insert(
        &mut self,
        path: Arc<RemoteConfigPath>,
        hash: String,
        meta: TargetFileMeta,
        version: u64,
        file: Arc<F>,
    ) -> (Arc<RemoteConfigPath>, Arc<F>) {
        let entry = self.0.entry(path);
        // The registry's copy, if the path is already known.
        let path = entry.key().clone();
        let versions = entry.or_default();
        let len = u64::try_from(meta.length).ok();
        if let Some(entry) = versions
            .iter_mut()
            .find(|entry| len.is_some_and(|len| entry.matches(&hash, len)))
        {
            entry.newest_version = entry.newest_version.max(version);
            return (path, entry.file.clone());
        }
        versions.push(VersionEntry {
            hash,
            meta,
            newest_version: version,
            file: file.clone(),
            expiring: false,
        });
        (path, file)
    }

    /// Records that a client now holds `file`, stored for `path`, as `version`: the entry is
    /// advertised again if it was expiring, and becomes the advertised version of its path if
    /// `version` is the newest a response named.
    pub(crate) fn hold(&mut self, path: &RemoteConfigPath, file: &Arc<F>, version: u64) {
        if let Some(entry) = self.find_mut(path, file) {
            entry.newest_version = entry.newest_version.max(version);
            entry.expiring = false;
        }
    }

    /// Keeps only the versions `keep` returns true for.
    pub(crate) fn retain(
        &mut self,
        mut keep: impl FnMut(&RemoteConfigPath, &VersionEntry<F>) -> bool,
    ) {
        self.0.retain(|path, versions| {
            versions.retain(|entry| keep(path, entry));
            !versions.is_empty()
        });
    }

    /// Keeps only the given files: those the only client using the registry holds.
    pub(crate) fn retain_held<'f>(&mut self, held: impl IntoIterator<Item = &'f Arc<F>>)
    where
        F: 'f,
    {
        let held: HashSet<*const F> = held.into_iter().map(Arc::as_ptr).collect();
        self.retain(|_, entry| held.contains(&Arc::as_ptr(&entry.file)));
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// Stops advertising `file`, stored for `path`. The predecessor to [`Self::expire`].
    pub(crate) fn mark_expiring(&mut self, path: &RemoteConfigPath, file: &Arc<F>) {
        if let Some(entry) = self.find_mut(path, file) {
            entry.expiring = true;
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// Removes `file`, stored for `path`, if it is still marked as expiring. Other versions of the
    /// path are untouched.
    pub(crate) fn expire(&mut self, path: &RemoteConfigPath, file: &Arc<F>) {
        let Some(versions) = self.0.get_mut(path) else {
            return;
        };
        versions.retain(|entry| !(entry.expiring && Arc::ptr_eq(&entry.file, file)));
        if versions.is_empty() {
            self.0.remove(path);
        }
    }

    fn find_mut(&mut self, path: &RemoteConfigPath, file: &Arc<F>) -> Option<&mut VersionEntry<F>> {
        self.0
            .get_mut(path)?
            .iter_mut()
            .find(|entry| Arc::ptr_eq(&entry.file, file))
    }
}

impl<F> VersionEntry<F> {
    /// Whether this is the version with the given primary hash and length.
    pub(crate) fn matches(&self, hash: &str, len: u64) -> bool {
        // A negative length should never appear; if it does, nothing matches it, so that the file
        // is fetched again.
        self.hash == hash && u64::try_from(self.meta.length).is_ok_and(|length| length == len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_trace_protobuf::remoteconfig::TargetFileHash;

    fn path() -> Arc<RemoteConfigPath> {
        Arc::new(RemoteConfigPath::parse("employee/APM_TRACING/1234/config").unwrap())
    }

    fn meta(path: &RemoteConfigPath, hash: &str, length: i64) -> TargetFileMeta {
        TargetFileMeta {
            path: path.to_string(),
            length,
            hashes: vec![TargetFileHash {
                algorithm: "sha256".to_string(),
                hash: hash.to_string(),
            }],
        }
    }

    fn insert(
        registry: &FileRegistry<String>,
        hash: &str,
        length: i64,
        version: u64,
        contents: &str,
    ) -> Arc<String> {
        let path = path();
        let meta = meta(&path, hash, length);
        let (_, file) = registry.lock().insert(
            path,
            hash.to_string(),
            meta,
            version,
            Arc::new(contents.to_string()),
        );
        file
    }

    fn advertised_hashes(registry: &FileRegistry<String>) -> Vec<String> {
        registry
            .advertise()
            .into_iter()
            .map(|meta| meta.hashes[0].hash.clone())
            .collect()
    }

    #[test]
    fn advertises_the_newest_version_of_each_path() {
        let registry = FileRegistry::default();
        insert(&registry, "h1", 2, 1, "v1");
        insert(&registry, "h2", 2, 2, "v2");
        assert_eq!(advertised_hashes(&registry), ["h2"]);
        assert_eq!(registry.paths(), 1);
        assert_eq!(
            registry
                .entries()
                .iter()
                .map(|e| (e.version, e.file.as_str()))
                .collect::<Vec<_>>(),
            [(1, "v1"), (2, "v2")],
            "both versions are kept"
        );
    }

    #[test]
    fn older_version_stored_later_keeps_the_newest_advertised() {
        let registry = FileRegistry::default();
        let v2 = insert(&registry, "h2", 2, 2, "v2");
        let v1 = insert(&registry, "h1", 2, 1, "v1");
        assert!(!Arc::ptr_eq(&v1, &v2));
        assert_eq!(advertised_hashes(&registry), ["h2"]);
        assert!(Arc::ptr_eq(
            &registry.lock().lookup(&path(), "h2", 2).unwrap().1,
            &v2
        ));
    }

    #[test]
    fn lookup_matches_hash_and_length() {
        let registry = FileRegistry::default();
        let v1 = insert(&registry, "h1", 2, 1, "v1");
        let guard = registry.lock();
        assert!(Arc::ptr_eq(&guard.lookup(&path(), "h1", 2).unwrap().1, &v1));
        assert!(guard.lookup(&path(), "h1", 3).is_none(), "length differs");
        assert!(guard.lookup(&path(), "h2", 2).is_none(), "hash differs");
        let other = RemoteConfigPath::parse("employee/APM_TRACING/9876/config").unwrap();
        assert!(guard.lookup(&other, "h1", 2).is_none(), "path differs");
    }

    #[test]
    fn version_only_change_keeps_the_entry() {
        let registry = FileRegistry::default();
        let v1 = insert(&registry, "h1", 2, 1, "v1");
        let v2 = insert(&registry, "h2", 2, 2, "v2");
        // The contents of v1 come back as version 3: the same entry, now the newest.
        let (path, found) = registry.lock().lookup(&path(), "h1", 2).unwrap();
        assert!(Arc::ptr_eq(&found, &v1));
        registry.lock().hold(&path, &found, 3);
        assert_eq!(advertised_hashes(&registry), ["h1"]);
        let again = insert(&registry, "h1", 2, 4, "v1 again");
        assert!(Arc::ptr_eq(&again, &v1), "never stored twice");
        assert_eq!(registry.entries().len(), 2);
        drop(v2);
    }

    #[test]
    fn empty_files_are_not_advertised() {
        let registry = FileRegistry::default();
        insert(&registry, "h1", 0, 1, "");
        assert!(registry.advertise().is_empty());
        assert!(registry.lock().lookup(&path(), "h1", 0).is_some());
    }

    #[test]
    fn expiring_versions_are_skipped_and_expired_by_identity() {
        let registry = FileRegistry::default();
        let v1 = insert(&registry, "h1", 2, 1, "v1");
        let v2 = insert(&registry, "h2", 2, 2, "v2");
        let path = path();

        registry.lock().mark_expiring(&path, &v2);
        assert_eq!(advertised_hashes(&registry), ["h1"]);

        // Not expiring: untouched.
        registry.lock().expire(&path, &v1);
        assert_eq!(registry.entries().len(), 2);

        registry.lock().expire(&path, &v2);
        assert_eq!(registry.entries().len(), 1);
        assert_eq!(advertised_hashes(&registry), ["h1"]);

        registry.lock().mark_expiring(&path, &v1);
        registry.lock().hold(&path, &v1, 1);
        assert_eq!(advertised_hashes(&registry), ["h1"], "holding revives it");
    }

    #[test]
    fn retain_held_keeps_only_held_versions() {
        let registry = FileRegistry::default();
        let v1 = insert(&registry, "h1", 2, 1, "v1");
        let v2 = insert(&registry, "h2", 2, 2, "v2");
        let released = Arc::downgrade(&v1);
        drop(v1);
        registry.lock().retain_held([&v2]);
        assert!(released.upgrade().is_none());
        assert_eq!(registry.entries().len(), 1);
        registry.lock().retain_held([]);
        assert!(registry.is_empty());
    }
}
