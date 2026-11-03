// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The file versions stored under one set of [`super::ConfigInvariants`], indexed by path and
//! hash. Several versions of a path can be alive at once, since each client keeps the version it
//! was last handed until its own next poll.
//!
//! The registry only observes the files: it keeps a [`Weak`] reference to each, and a version is
//! alive exactly as long as something holds a strong one: the holdings of the clients holding
//! it, the [`RequestSnapshot`]s of the requests which advertised it, and whatever a fetch or a
//! consumer is still working with. Dead entries are pruned lazily.
//!
//! Every method takes the lock briefly and never drops a strong reference while holding it, so
//! that a file's `Drop` (e.g. unlinking a shared memory segment) never runs under the lock. Files
//! are stored without holding it either: only the version being stored is locked, so that
//! different versions are stored in parallel.
//!
//! This module is free of any async runtime, so that it builds wherever the fetcher does.

use crate::RemoteConfigPath;
use hashbrown::{HashMap, HashSet};
use libdd_common::MutexExt;
use libdd_trace_protobuf::remoteconfig::TargetFileMeta;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

/// The stored file versions, by path. A version is identified by (path, hash, length), which is
/// what the agent deduplicates on: its version number plays no part in it. A version-only change
/// therefore maps to the same entry.
///
/// Storing a version never replaces another live one. Which version of a path is advertised is
/// computed from the versions responses named each entry with, so a client being served an older
/// version cannot displace the newest one.
pub(crate) struct FileRegistry<F> {
    paths: Mutex<Versions<F>>,
}

type Versions<F> = HashMap<Arc<RemoteConfigPath>, Vec<VersionEntry<F>>>;

/// One stored version: one (path, hash, length). While its file is not alive, it is only a
/// placeholder for stores in progress, which is neither advertised nor found.
struct VersionEntry<F> {
    /// The primary hash of the contents, as chosen by the caller (the agent protocol uses sha256,
    /// else sha512).
    hash: String,
    /// What is advertised for this version: path, length and all hashes.
    meta: TargetFileMeta,
    /// The highest version a response named this entry with.
    newest_version: u64,
    /// The only reference the registry keeps.
    file: Weak<F>,
    /// Held across the store of this version, so that it is never stored twice at once. The
    /// entry is kept, even with a dead file, as long as a store holds or waits for it: cloned
    /// only under the registry lock, a count of one means none does nor can start to.
    storing: Arc<Mutex<()>>,
    /// The number of live [`RequestSnapshot`]s which advertised this version.
    in_flight: Arc<AtomicU32>,
}

/// What a request advertised as cached, and strong references to those versions: they stay
/// alive until the response to that request has been processed, so that a response listing one
/// of them without a body still finds it. Dropping the snapshot releases them.
pub(crate) struct RequestSnapshot<F> {
    /// For `cached_target_files`.
    pub(crate) cached: Vec<TargetFileMeta>,
    retained: Vec<(Arc<F>, Arc<AtomicU32>)>,
}

impl<F> Drop for RequestSnapshot<F> {
    fn drop(&mut self) {
        for (file, in_flight) in self.retained.drain(..) {
            // Released first, so that the version is never counted as held by something else
            // than requests once they no longer hold it.
            drop(file);
            in_flight.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Counts of the live versions, see [`FileRegistry::stats`].
pub(crate) struct RegistryStats {
    /// Paths with a live version.
    pub paths: u32,
    /// Live versions, of all paths.
    pub live: u32,
    /// Live versions retained only by in-flight requests.
    pub in_flight_only: u32,
}

impl<F> Default for FileRegistry<F> {
    fn default() -> Self {
        FileRegistry {
            paths: Default::default(),
        }
    }
}

impl<F> FileRegistry<F> {
    /// The versions to list in `cached_target_files`, and strong references keeping them alive
    /// until the returned snapshot is dropped: per path, the live version most recently named by
    /// a response.
    ///
    /// Older live versions are not listed: the agent only compares cached files with its current
    /// targets, which name the newest one. Neither are versions only alive for in-flight
    /// requests: listing those would let overlapping requests keep them alive indefinitely. They
    /// are still found by [`Self::lookup`] while alive, should a response name them.
    ///
    /// Empty files are not listed: the agent rejects a request listing a cached file whose length
    /// is below 1, and not listing them only makes it resend their empty body.
    pub(crate) fn advertise(&self) -> RequestSnapshot<F> {
        let mut cached = vec![];
        let mut retained = vec![];
        let mut paths = self.paths.lock_or_panic();
        prune(&mut paths);
        for versions in paths.values() {
            let Some(entry) = versions
                .iter()
                .filter(|entry| entry.is_held_beyond_requests())
                .max_by_key(|entry| entry.newest_version)
            else {
                continue;
            };
            if entry.meta.length <= 0 {
                continue;
            }
            // Died since checked: not listed.
            let Some(file) = entry.file.upgrade() else {
                continue;
            };
            entry.in_flight.fetch_add(1, Ordering::AcqRel);
            retained.push((file, entry.in_flight.clone()));
            cached.push(entry.meta.clone());
        }
        drop(paths);
        RequestSnapshot { cached, retained }
    }

    /// The live version of `path` with the given primary hash and length, if any, along with
    /// the registry's copy of the path.
    pub(crate) fn lookup(
        &self,
        path: &RemoteConfigPath,
        hash: &str,
        len: u64,
    ) -> Option<(Arc<RemoteConfigPath>, Arc<F>)> {
        let paths = self.paths.lock_or_panic();
        let (path, versions) = paths.get_key_value(path)?;
        let file = versions
            .iter()
            .find(|entry| entry.matches(hash, len))?
            .file
            .upgrade()?;
        Some((path.clone(), file))
    }

    /// The live version of `path` with the given primary hash and length `meta.length`, or, if
    /// there is none, the one `store` returns, recorded as named `version` by a response. Returns
    /// the registry's copy of the path and the stored file.
    ///
    /// `store` is called with only this version locked: fetchers sharing the registry never store
    /// the same version twice, and one waiting for another to store it reuses that file. It must
    /// not call back into the registry for the same version.
    pub(crate) fn get_or_store(
        &self,
        path: Arc<RemoteConfigPath>,
        hash: String,
        meta: TargetFileMeta,
        version: u64,
        store: impl FnOnce() -> anyhow::Result<Arc<F>>,
    ) -> anyhow::Result<(Arc<RemoteConfigPath>, Arc<F>)> {
        let Ok(len) = u64::try_from(meta.length) else {
            // Never matched (see VersionEntry::matches), so there is nothing to wait for.
            let file = store()?;
            let mut paths = self.paths.lock_or_panic();
            let entry = paths.entry(path);
            let path = entry.key().clone();
            entry.or_default().push(VersionEntry::new(
                hash,
                meta,
                version,
                Arc::downgrade(&file),
                Arc::default(),
            ));
            return Ok((path, file));
        };

        // The registry's copy of the path, and the lock of the entry, created if missing.
        let (path, storing) = {
            let mut paths = self.paths.lock_or_panic();
            let entry = paths.entry(path);
            let path = entry.key().clone();
            let versions = entry.or_default();
            let storing = match versions.iter().find(|entry| entry.matches(&hash, len)) {
                Some(entry) => match entry.file.upgrade() {
                    Some(file) => return Ok((path, file)),
                    None => entry.storing.clone(),
                },
                None => {
                    let storing = Arc::<Mutex<()>>::default();
                    versions.push(VersionEntry::new(
                        hash.clone(),
                        meta.clone(),
                        version,
                        Weak::new(),
                        storing.clone(),
                    ));
                    storing
                }
            };
            (path, storing)
        };

        // Nothing protected: the lock only serialises stores, so a panicking one poisons nothing.
        let _storing = storing.lock().unwrap_or_else(PoisonError::into_inner);
        // Stored by whoever held the lock before?
        let stored = {
            let paths = self.paths.lock_or_panic();
            find_storing(&paths, &path, &storing).and_then(|entry| entry.file.upgrade())
        };
        if let Some(file) = stored {
            return Ok((path, file));
        }

        let result = store();
        let mut paths = self.paths.lock_or_panic();
        let versions = paths.entry(path.clone()).or_default();
        let entry = versions
            .iter()
            .position(|entry| Arc::ptr_eq(&entry.storing, &storing));
        match result {
            Ok(file) => {
                let stored =
                    VersionEntry::new(hash, meta, version, Arc::downgrade(&file), storing.clone());
                match entry {
                    Some(entry) => versions[entry] = stored,
                    // Forgotten (see Self::clear) while storing.
                    None => versions.push(stored),
                }
                Ok((path, file))
            }
            Err(e) => {
                // Nobody else holds nor waits for the lock: none will store this version, so the
                // placeholder goes. A waiter would try storing it in turn.
                let unused = entry.filter(|&entry| {
                    Arc::strong_count(&storing) == 2 && !versions[entry].is_alive()
                });
                if let Some(entry) = unused {
                    versions.swap_remove(entry);
                }
                if versions.is_empty() {
                    paths.remove(&path);
                }
                Err(e)
            }
        }
    }

    /// Records that a client now holds each given file, stored for its path, as the given
    /// version: it becomes the advertised version of its path if that is the newest version a
    /// response named.
    pub(crate) fn hold<'f>(
        &self,
        held: impl IntoIterator<Item = (&'f RemoteConfigPath, &'f Arc<F>, u64)>,
    ) where
        F: 'f,
    {
        let mut paths = self.paths.lock_or_panic();
        for (path, file, version) in held {
            if let Some(entry) = paths
                .get_mut(path)
                .and_then(|versions| versions.iter_mut().find(|entry| entry.is(file)))
            {
                entry.newest_version = entry.newest_version.max(version);
            }
        }
    }

    /// Keeps only the given files: those the only client using the registry holds.
    pub(crate) fn retain_held<'f>(&self, held: impl IntoIterator<Item = &'f Arc<F>>)
    where
        F: 'f,
    {
        let held: HashSet<*const F> = held.into_iter().map(Arc::as_ptr).collect();
        self.paths.lock_or_panic().retain(|_, versions| {
            versions.retain(|entry| held.contains(&entry.file.as_ptr()) || entry.is_being_stored());
            !versions.is_empty()
        });
    }

    /// Forgets every version. Versions still alive are no longer advertised nor found.
    pub(crate) fn clear(&self) {
        self.paths.lock_or_panic().clear();
    }

    /// Counts the live versions. Those retained only by in-flight requests are counted as of
    /// when checked: it is racy, but cheap.
    pub(crate) fn stats(&self) -> RegistryStats {
        let mut paths = self.paths.lock_or_panic();
        prune(&mut paths);
        let mut stats = RegistryStats {
            paths: 0,
            live: 0,
            in_flight_only: 0,
        };
        for versions in paths.values() {
            let mut live = versions.iter().filter(|entry| entry.is_alive()).peekable();
            if live.peek().is_some() {
                stats.paths += 1;
            }
            for entry in live {
                stats.live += 1;
                if !entry.is_held_beyond_requests() {
                    stats.in_flight_only += 1;
                }
            }
        }
        stats
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        let mut paths = self.paths.lock_or_panic();
        prune(&mut paths);
        paths.is_empty()
    }

    /// Every live version, sorted by path, then version.
    #[cfg(test)]
    pub(crate) fn entries(&self) -> Vec<EntryInfo<F>> {
        let mut entries: Vec<_> = self
            .paths
            .lock_or_panic()
            .iter()
            .flat_map(|(path, versions)| {
                versions.iter().filter_map(|entry| {
                    Some(EntryInfo {
                        path: path.to_string(),
                        hash: entry.hash.clone(),
                        version: entry.newest_version,
                        file: entry.file.upgrade()?,
                    })
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
    pub file: Arc<F>,
}

/// Drops the entries of dead versions no store is in progress for, and the paths left without
/// any.
fn prune<F>(paths: &mut Versions<F>) {
    paths.retain(|_, versions| {
        versions.retain(|entry| entry.is_alive() || entry.is_being_stored());
        !versions.is_empty()
    });
}

/// The entry of `path` locked by `storing`, if still registered.
fn find_storing<'v, F>(
    paths: &'v Versions<F>,
    path: &RemoteConfigPath,
    storing: &Arc<Mutex<()>>,
) -> Option<&'v VersionEntry<F>> {
    paths
        .get(path)?
        .iter()
        .find(|entry| Arc::ptr_eq(&entry.storing, storing))
}

impl<F> VersionEntry<F> {
    fn new(
        hash: String,
        meta: TargetFileMeta,
        version: u64,
        file: Weak<F>,
        storing: Arc<Mutex<()>>,
    ) -> Self {
        VersionEntry {
            hash,
            meta,
            newest_version: version,
            file,
            storing,
            // Fresh: requests which advertised a previous life of the entry no longer count.
            in_flight: Default::default(),
        }
    }

    /// Whether this is the version with the given primary hash and length.
    fn matches(&self, hash: &str, len: u64) -> bool {
        // A negative length should never appear; if it does, nothing matches it, so that the file
        // is fetched again.
        self.hash == hash && u64::try_from(self.meta.length).is_ok_and(|length| length == len)
    }

    fn is(&self, file: &Arc<F>) -> bool {
        std::ptr::eq(self.file.as_ptr(), Arc::as_ptr(file))
    }

    fn is_alive(&self) -> bool {
        self.file.strong_count() > 0
    }

    /// Whether a store of this version holds or waits for its lock. Read under the registry
    /// lock, `false` is final: the lock is only cloned under it.
    fn is_being_stored(&self) -> bool {
        Arc::strong_count(&self.storing) > 1
    }

    /// Whether something else than in-flight requests keeps this version alive.
    fn is_held_beyond_requests(&self) -> bool {
        self.file.strong_count() > self.in_flight.load(Ordering::Acquire) as usize
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
        let (_, file) = registry
            .get_or_store(path, hash.to_string(), meta, version, || {
                Ok(Arc::new(contents.to_string()))
            })
            .unwrap();
        file
    }

    fn advertised_hashes(registry: &FileRegistry<String>) -> Vec<String> {
        registry
            .advertise()
            .cached
            .iter()
            .map(|meta| meta.hashes[0].hash.clone())
            .collect()
    }

    #[test]
    fn advertises_the_newest_version_of_each_path() {
        let registry = FileRegistry::default();
        let _v1 = insert(&registry, "h1", 2, 1, "v1");
        let _v2 = insert(&registry, "h2", 2, 2, "v2");
        assert_eq!(advertised_hashes(&registry), ["h2"]);
        assert_eq!(registry.stats().paths, 1);
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
            &registry.lookup(&path(), "h2", 2).unwrap().1,
            &v2
        ));
    }

    #[test]
    fn lookup_matches_hash_and_length() {
        let registry = FileRegistry::default();
        let v1 = insert(&registry, "h1", 2, 1, "v1");
        assert!(Arc::ptr_eq(
            &registry.lookup(&path(), "h1", 2).unwrap().1,
            &v1
        ));
        assert!(
            registry.lookup(&path(), "h1", 3).is_none(),
            "length differs"
        );
        assert!(registry.lookup(&path(), "h2", 2).is_none(), "hash differs");
        let other = RemoteConfigPath::parse("employee/APM_TRACING/9876/config").unwrap();
        assert!(registry.lookup(&other, "h1", 2).is_none(), "path differs");
    }

    #[test]
    fn version_only_change_keeps_the_entry() {
        let registry = FileRegistry::default();
        let v1 = insert(&registry, "h1", 2, 1, "v1");
        let _v2 = insert(&registry, "h2", 2, 2, "v2");
        // The contents of v1 come back as version 3: the same entry, now the newest.
        let (path, found) = registry.lookup(&path(), "h1", 2).unwrap();
        assert!(Arc::ptr_eq(&found, &v1));
        registry.hold([(&*path, &found, 3)]);
        assert_eq!(advertised_hashes(&registry), ["h1"]);
        let again = insert(&registry, "h1", 2, 4, "v1 again");
        assert!(Arc::ptr_eq(&again, &v1), "never stored twice");
        assert_eq!(registry.entries().len(), 2);
    }

    #[test]
    fn empty_files_are_not_advertised() {
        let registry = FileRegistry::default();
        let _empty = insert(&registry, "h1", 0, 1, "");
        assert!(registry.advertise().cached.is_empty());
        assert!(registry.lookup(&path(), "h1", 0).is_some());
    }

    #[test]
    fn released_versions_are_forgotten_and_stored_again() {
        let registry = FileRegistry::default();
        let v1 = insert(&registry, "h1", 2, 1, "v1");
        let v2 = insert(&registry, "h2", 2, 2, "v2");
        drop(v2);
        assert_eq!(
            advertised_hashes(&registry),
            ["h1"],
            "the live one is advertised"
        );
        assert!(registry.lookup(&path(), "h2", 2).is_none());
        assert_eq!(registry.entries().len(), 1);

        let stored_again = insert(&registry, "h2", 2, 2, "v2 again");
        assert_eq!(*stored_again, "v2 again");
        drop((v1, stored_again));
        assert!(registry.is_empty());
    }

    #[test]
    fn failed_store_leaves_nothing() {
        let registry = FileRegistry::<String>::default();
        let meta = meta(&path(), "h1", 2);
        assert!(
            registry
                .get_or_store(path(), "h1".to_string(), meta, 1, || anyhow::bail!("full"))
                .is_err()
        );
        assert!(registry.paths.lock_or_panic().is_empty());
        assert!(registry.advertise().cached.is_empty());
        assert!(registry.lookup(&path(), "h1", 2).is_none());

        let stored = insert(&registry, "h1", 2, 1, "v1");
        assert_eq!(*stored, "v1", "stored on a later attempt");
        assert_eq!(advertised_hashes(&registry), ["h1"]);
    }

    #[test]
    fn snapshots_keep_advertised_versions_alive() {
        let registry = FileRegistry::default();
        let v1 = insert(&registry, "h1", 2, 1, "v1");
        let released = Arc::downgrade(&v1);
        let snapshot = registry.advertise();
        assert_eq!(snapshot.cached.len(), 1);
        drop(v1);

        // Only the request keeps it: still found, but neither advertised nor counted as held.
        assert!(registry.lookup(&path(), "h1", 2).is_some());
        assert!(registry.advertise().cached.is_empty());
        let stats = registry.stats();
        assert_eq!((stats.paths, stats.live, stats.in_flight_only), (1, 1, 1));

        drop(snapshot);
        assert!(released.upgrade().is_none());
        let stats = registry.stats();
        assert_eq!((stats.paths, stats.live, stats.in_flight_only), (0, 0, 0));
    }

    #[test]
    fn retain_held_keeps_only_held_versions() {
        let registry = FileRegistry::default();
        let v1 = insert(&registry, "h1", 2, 1, "v1");
        let v2 = insert(&registry, "h2", 2, 2, "v2");
        registry.retain_held([&v2]);
        assert!(registry.lookup(&path(), "h1", 2).is_none());
        assert_eq!(registry.entries().len(), 1);
        registry.retain_held([]);
        assert!(registry.is_empty());
        drop(v1);
    }

    #[test]
    fn storing_one_version_does_not_block_storing_another() {
        let registry = Arc::new(FileRegistry::<String>::default());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let slow = std::thread::spawn({
            let registry = registry.clone();
            move || {
                let path = path();
                let meta = meta(&path, "h1", 2);
                registry
                    .get_or_store(path, "h1".to_string(), meta, 1, || {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(Arc::new("v1".to_string()))
                    })
                    .unwrap()
                    .1
            }
        });
        entered_rx.recv().unwrap();

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let fast = std::thread::spawn({
            let registry = registry.clone();
            move || {
                let file = insert(&registry, "h2", 2, 2, "v2");
                done_tx.send(()).unwrap();
                file
            }
        });
        let fast_done = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_ok();
        release_tx.send(()).unwrap();
        let v1 = slow.join().unwrap();
        let v2 = fast.join().unwrap();
        assert!(fast_done, "storing v2 waited for the store of v1");
        assert_eq!((v1.as_str(), v2.as_str()), ("v1", "v2"));
        assert_eq!(advertised_hashes(&registry), ["h2"]);
    }

    /// The holders and waiters of the lock of the entry with the given hash, the entry included.
    fn storing_count(registry: &FileRegistry<String>, hash: &str) -> usize {
        registry.paths.lock_or_panic()[&*path()]
            .iter()
            .find(|entry| entry.hash == hash)
            .map_or(0, |entry| Arc::strong_count(&entry.storing))
    }

    /// Spawns a `get_or_store` of version 1 of "h1", whose store signals `entered`, then returns
    /// what it receives from `release` (Err: fails).
    fn spawn_store(
        registry: &Arc<FileRegistry<String>>,
        stores: &Arc<std::sync::atomic::AtomicUsize>,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<Result<String, String>>,
    ) -> std::thread::JoinHandle<anyhow::Result<Arc<String>>> {
        let registry = registry.clone();
        let stores = stores.clone();
        std::thread::spawn(move || {
            let path = path();
            let meta = meta(&path, "h1", 2);
            registry
                .get_or_store(path, "h1".to_string(), meta, 1, || {
                    stores.fetch_add(1, Ordering::SeqCst);
                    entered.send(()).unwrap();
                    match release.recv().unwrap() {
                        Ok(contents) => Ok(Arc::new(contents)),
                        Err(e) => Err(anyhow::anyhow!(e)),
                    }
                })
                .map(|(_, file)| file)
        })
    }

    /// Waits until `count` stores hold or wait for the lock of "h1".
    fn wait_for_stores(registry: &FileRegistry<String>, count: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while storing_count(registry, "h1") != count + 1 {
            assert!(std::time::Instant::now() < deadline, "stores never met");
            std::thread::yield_now();
        }
    }

    #[test]
    fn concurrent_stores_of_one_version_store_it_once() {
        let registry = Arc::new(FileRegistry::<String>::default());
        let stores = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first = spawn_store(&registry, &stores, entered_tx.clone(), release_rx);
        entered_rx.recv().unwrap();
        // Would store "second" if it stored at all.
        let (second_release_tx, second_release_rx) = std::sync::mpsc::channel();
        second_release_tx.send(Ok("second".to_string())).unwrap();
        let second = spawn_store(&registry, &stores, entered_tx, second_release_rx);
        // The second is waiting for the first to store the version.
        wait_for_stores(&registry, 2);
        assert!(advertised_hashes(&registry).is_empty(), "not stored yet");
        release_tx.send(Ok("first".to_string())).unwrap();

        let first = first.join().unwrap().unwrap();
        let second = second.join().unwrap().unwrap();
        assert_eq!(stores.load(Ordering::SeqCst), 1);
        assert_eq!(*first, "first");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(registry.entries().len(), 1);
        assert_eq!(advertised_hashes(&registry), ["h1"]);
    }

    #[test]
    fn failed_store_lets_a_waiting_one_store() {
        let registry = Arc::new(FileRegistry::<String>::default());
        let stores = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let failing = spawn_store(&registry, &stores, entered_tx.clone(), release_rx);
        entered_rx.recv().unwrap();
        let (second_release_tx, second_release_rx) = std::sync::mpsc::channel();
        let second = spawn_store(&registry, &stores, entered_tx, second_release_rx);
        wait_for_stores(&registry, 2);
        release_tx.send(Err("full".to_string())).unwrap();
        assert!(failing.join().unwrap().is_err());

        // The waiting one stores it in turn.
        entered_rx.recv().unwrap();
        assert!(advertised_hashes(&registry).is_empty(), "not stored yet");
        assert_eq!(registry.stats().live, 0);
        second_release_tx.send(Ok("second".to_string())).unwrap();
        let second = second.join().unwrap().unwrap();
        assert_eq!(stores.load(Ordering::SeqCst), 2);
        assert_eq!(*second, "second");
        assert_eq!(registry.paths.lock_or_panic()[&*path()].len(), 1);
        assert_eq!(advertised_hashes(&registry), ["h1"]);
        drop(second);
        assert!(registry.is_empty());
    }
}
