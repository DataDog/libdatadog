// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! LRU cache wrapper with dual limits: maximum entry count AND maximum tracked byte size.
//!
//! `lru::LruCache` only supports an entry-count capacity, which is unsafe for caches keyed on
//! arbitrary user strings: a few very large keys can balloon memory. `BoundedByteCache` adds
//! a byte budget on top, evicting least-recently-used entries until both limits are satisfied.

use lru::LruCache;
use std::borrow::Borrow;
use std::hash::Hash;
use std::mem::size_of;
use std::num::NonZeroUsize;

/// Default maximum entry count.
pub const DEFAULT_MAX_ENTRIES: usize = 256;

/// Default maximum tracked byte size (256 KiB).
pub const DEFAULT_MAX_BYTES: usize = 256 * 1024;

/// LRU cache bounded by both entry count and total tracked byte size.
///
/// Byte accounting covers `key.as_ref().len() + size_of::<V>()`. Heap-allocated value contents
/// are not tracked; this wrapper assumes small inline values (e.g. `bool`).
pub struct BoundedByteCache<K, V>
where
    K: Hash + Eq + AsRef<[u8]>,
{
    inner: LruCache<K, V>,
    current_bytes: usize,
    max_bytes: usize,
}

impl<K, V> BoundedByteCache<K, V>
where
    K: Hash + Eq + AsRef<[u8]>,
{
    /// `max_entries` of zero is treated as 1 (a cache with no slots is nonsensical).
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        let entry_cap = NonZeroUsize::new(max_entries).unwrap_or(NonZeroUsize::MIN);
        Self {
            inner: LruCache::new(entry_cap),
            current_bytes: 0,
            max_bytes,
        }
    }

    #[inline]
    pub fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.get(key)
    }

    /// Insert `key -> value`. Entries larger than `max_bytes` are dropped silently. Otherwise
    /// LRU entries are evicted until the new entry fits.
    #[inline]
    pub fn put(&mut self, key: K, value: V) {
        let entry_bytes = Self::entry_size(&key);

        if entry_bytes > self.max_bytes {
            return;
        }

        // Replacing an existing key: deduct its bytes first.
        if self.inner.pop(&key).is_some() {
            self.current_bytes = self.current_bytes.saturating_sub(entry_bytes);
        }

        while self.current_bytes + entry_bytes > self.max_bytes {
            match self.inner.pop_lru() {
                Some((evicted_key, _)) => {
                    self.current_bytes = self
                        .current_bytes
                        .saturating_sub(Self::entry_size(&evicted_key));
                }
                None => break,
            }
        }

        // `push` may evict an LRU entry to honor the entry-count cap; deduct its bytes.
        if let Some((replaced_key, _)) = self.inner.push(key, value) {
            self.current_bytes = self
                .current_bytes
                .saturating_sub(Self::entry_size(&replaced_key));
        }
        self.current_bytes += entry_bytes;
    }

    #[cfg(test)]
    pub fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    fn entry_size(key: &K) -> usize {
        Self::PER_ENTRY_OVERHEAD + key.as_ref().len() + size_of::<V>()
    }

    /// Approximate per-entry fixed heap overhead, in bytes. Covers:
    /// - `Vec<u8>` header on the key (24 B on 64-bit targets)
    /// - `lru` doubly-linked-list node (prev/next pointers, ~24 B)
    /// - `HashMap` bucket amortized (~16 B)
    ///
    /// Rounded up so `max_bytes` is a pessimistic upper bound on actual heap usage. Recheck
    /// if the `lru` crate is upgraded across a major version — the linked-list node layout
    /// is the volatile piece.
    const PER_ENTRY_OVERHEAD: usize = 64;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_put_and_get() {
        let mut cache: BoundedByteCache<Vec<u8>, bool> = BoundedByteCache::new(256, 1024);
        cache.put(b"hello".to_vec(), true);
        assert_eq!(cache.get(b"hello".as_ref()), Some(&true));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_evicts_lru_when_over_byte_budget() {
        // Each entry costs PER_ENTRY_OVERHEAD (64) + 4 (key) + 1 (bool) = 69 bytes. Budget
        // of 150 fits two entries (138 B) but not three.
        let budget = 150;
        let mut cache: BoundedByteCache<Vec<u8>, bool> = BoundedByteCache::new(256, budget);
        cache.put(b"aaaa".to_vec(), true);
        cache.put(b"bbbb".to_vec(), false);
        assert_eq!(cache.len(), 2);
        cache.put(b"cccc".to_vec(), true);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(b"aaaa".as_ref()), None);
        assert_eq!(cache.get(b"bbbb".as_ref()), Some(&false));
        assert_eq!(cache.get(b"cccc".as_ref()), Some(&true));
        assert!(cache.current_bytes() <= budget);
    }

    #[test]
    fn test_evicts_lru_when_over_entry_count() {
        // Generous byte budget; entry-count cap of 2 drives eviction.
        let mut cache: BoundedByteCache<Vec<u8>, bool> = BoundedByteCache::new(2, 1024);
        cache.put(b"a".to_vec(), true);
        cache.put(b"b".to_vec(), false);
        cache.put(b"c".to_vec(), true);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(b"a".as_ref()), None);
        assert_eq!(cache.get(b"b".as_ref()), Some(&false));
        assert_eq!(cache.get(b"c".as_ref()), Some(&true));
    }

    #[test]
    fn test_oversize_entry_is_rejected() {
        // Any entry costs at least PER_ENTRY_OVERHEAD bytes, so a 32-byte budget rejects
        // every insertion.
        let mut cache: BoundedByteCache<Vec<u8>, bool> = BoundedByteCache::new(256, 32);
        cache.put(b"small".to_vec(), true);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.current_bytes(), 0);
    }

    #[test]
    fn test_replacing_key_does_not_double_count() {
        let mut cache: BoundedByteCache<Vec<u8>, bool> = BoundedByteCache::new(256, 1024);
        cache.put(b"k".to_vec(), true);
        let bytes_after_first = cache.current_bytes();
        cache.put(b"k".to_vec(), false);
        assert_eq!(cache.current_bytes(), bytes_after_first);
        assert_eq!(cache.get(b"k".as_ref()), Some(&false));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_get_bumps_recency() {
        // Budget for exactly two 4-byte-keyed entries (69 B each = 138 B total).
        let mut cache: BoundedByteCache<Vec<u8>, bool> = BoundedByteCache::new(256, 150);
        cache.put(b"aaaa".to_vec(), true);
        cache.put(b"bbbb".to_vec(), true);
        let _ = cache.get(b"aaaa".as_ref());
        cache.put(b"cccc".to_vec(), true);
        assert_eq!(cache.get(b"aaaa".as_ref()), Some(&true));
        assert_eq!(cache.get(b"bbbb".as_ref()), None);
    }

    #[test]
    fn test_many_inserts_stay_within_both_limits() {
        let max_entries = 8;
        // 8 entries * (PER_ENTRY_OVERHEAD 64 + 8-byte key + 1) = 584 bytes; round up.
        let max_bytes = 600;
        let mut cache: BoundedByteCache<Vec<u8>, bool> =
            BoundedByteCache::new(max_entries, max_bytes);
        for i in 0u16..1000 {
            cache.put(format!("key-{:04}", i).into_bytes(), i % 2 == 0);
            assert!(cache.current_bytes() <= max_bytes);
            assert!(cache.len() <= max_entries);
        }
    }

    #[test]
    fn test_zero_entries_clamps_to_one() {
        let mut cache: BoundedByteCache<Vec<u8>, bool> = BoundedByteCache::new(0, 1024);
        cache.put(b"a".to_vec(), true);
        cache.put(b"b".to_vec(), false);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(b"a".as_ref()), None);
        assert_eq!(cache.get(b"b".as_ref()), Some(&false));
    }
}
