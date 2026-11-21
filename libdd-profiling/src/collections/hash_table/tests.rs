// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::group::{BitMask, Group, scalar};
use super::*;
use rust_alloc::vec::Vec;

/// 16-aligned backing bytes, standing in for any caller-provided memory.
#[repr(C, align(16))]
struct Bytes<const N: usize>([u8; N]);

impl<const N: usize> Bytes<N> {
    const fn new() -> Self {
        Self([0; N])
    }
}

/// A reasonable 64-bit mix so tags and start groups vary.
fn hash(key: u64) -> u64 {
    let mut x = key.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn intern(table: &mut HashTable<'_, u64>, key: u64) -> Option<bool> {
    match table.entry(hash(key), |&stored| stored == key) {
        Entry::Occupied(&stored) => {
            assert_eq!(stored, key);
            Some(false)
        }
        Entry::Vacant(entry) => {
            entry.insert(key);
            Some(true)
        }
        Entry::Full => None,
    }
}

#[test]
fn simd_matches_scalar_for_every_empty_pattern() {
    // Native runs cover every pattern; Miri samples them (plus the all-full
    // pattern) to keep its runtime practical.
    let stride = if cfg!(miri) { 4093 } else { 1 };
    for empty_pattern in (0..=u16::MAX).step_by(stride).chain([u16::MAX]) {
        let mut group = Group::EMPTY;
        for (lane, control) in group.0.iter_mut().enumerate() {
            if empty_pattern & (1 << lane) == 0 {
                *control = [0x80, 0x91, 0xff][lane % 3];
            }
        }
        for tag in [0x00, 0x80, 0x91, 0xff, 0xa5] {
            assert_eq!(group.match_tag(tag), scalar::match_byte(&group, tag));
        }
        assert_eq!(group.match_empty(), BitMask(empty_pattern));
        assert_eq!(group.match_full(), BitMask(!empty_pattern));
        assert_eq!(group.match_full(), scalar::match_high_bit(&group));
    }
}

#[test]
fn bitmask_iterates_set_bits_in_order() {
    let bits: Vec<usize> = BitMask(0b1000_0000_0010_0101).into_iter().collect();
    assert_eq!(bits, [0, 2, 5, 15]);
    assert_eq!(BitMask(0).lowest(), None);
    assert_eq!(BitMask(0x8000).lowest(), Some(15));
}

#[test]
fn geometry_matches_dictionary_layouts() {
    // 8-byte string buckets and 4-byte fixed-table buckets.
    assert_eq!(bytes_per_group::<[u32; 2]>(), 144);
    assert_eq!(bytes_per_group::<u32>(), 80);
    // One 16 KiB block minus the 64-byte region prefix.
    assert_eq!(groups_for_bytes::<[u32; 2]>(16 * 1024 - 64), 113);
    assert_eq!(capacity_for_groups(113), 1582);
    assert_eq!(bytes_for_groups::<u64>(usize::MAX), None);
}

#[test]
fn inserts_deduplicates_and_finds() {
    let mut bytes = Bytes::<{ 64 * 144 }>::new();
    let groups = groups_for_bytes::<u64>(bytes.0.len());
    let mut table = HashTable::<u64>::new(&mut bytes.0).unwrap();
    let capacity = table.capacity();
    assert_eq!(capacity, groups * GROUP_CAPACITY);

    for key in 0..capacity as u64 {
        assert_eq!(intern(&mut table, key), Some(true), "key {key}");
    }
    assert!(table.is_full());
    for key in 0..capacity as u64 {
        assert_eq!(intern(&mut table, key), Some(false), "duplicate {key}");
        assert_eq!(table.find(hash(key), |&s| s == key), Some(&key));
    }
    assert_eq!(table.len(), capacity);

    // Full: new keys are refused without changing the table, existing keys
    // are still found.
    assert_eq!(intern(&mut table, u64::MAX), None);
    assert_eq!(table.insert_unique(hash(u64::MAX), u64::MAX), Err(u64::MAX));
    assert_eq!(table.len(), capacity);
    assert_eq!(table.find(hash(u64::MAX), |&s| s == u64::MAX), None);
}

#[test]
fn iteration_yields_every_entry_once() {
    let mut bytes = Bytes::<{ 8 * 144 }>::new();
    let mut table = HashTable::<u64>::new(&mut bytes.0).unwrap();
    for key in 0..100 {
        intern(&mut table, key * 7);
    }
    let mut seen: Vec<u64> = table.iter().copied().collect();
    seen.sort_unstable();
    assert_eq!(seen, (0..100).map(|k| k * 7).collect::<Vec<_>>());
}

#[test]
fn reopen_and_view_see_existing_entries() {
    let mut bytes = Bytes::<{ 4 * 144 }>::new();
    {
        let mut table = HashTable::<u64>::new(&mut bytes.0).unwrap();
        for key in 0..40 {
            intern(&mut table, key);
        }
    }
    {
        let view = HashTableRef::<u64>::new(&bytes.0, 4).unwrap();
        assert_eq!(view.iter().count(), 40);
        assert_eq!(view.find(hash(39), |&s| s == 39), Some(&39));
        assert_eq!(view.find(hash(40), |&s| s == 40), None);
    }
    let mut table = HashTable::<u64>::reopen(&mut bytes.0, 4, 40).unwrap();
    assert_eq!(intern(&mut table, 5), Some(false));
    assert_eq!(intern(&mut table, 40), Some(true));
    assert_eq!(table.len(), 41);
}

#[test]
fn from_zeroed_uses_zeroed_bytes_as_is() {
    let mut bytes = Bytes::<{ 2 * 80 }>::new();
    let mut table = HashTable::<u32>::from_zeroed(&mut bytes.0).unwrap();
    assert_eq!(table.groups(), 2);
    assert_eq!(table.iter().count(), 0);
    assert_eq!(table.insert_unique(hash(1), 1), Ok(()));
    assert_eq!(table.find(hash(1), |&s| s == 1), Some(&1));
}

#[test]
fn from_zeroed_does_not_zero_and_new_does() {
    let mut bytes = Bytes::<{ 2 * 80 }>::new();
    bytes.0.fill(0xff);
    // Every control byte is 0xff, an occupied tag: from_zeroed trusts it.
    let table = HashTable::<u32>::from_zeroed(&mut bytes.0).unwrap();
    assert_eq!(table.iter().count(), 32);
    let table = HashTable::<u32>::new(&mut bytes.0).unwrap();
    assert_eq!(table.iter().count(), 0);
    assert!(bytes.0[..32].iter().all(|&b| b == 0));
    // Buckets are not touched.
    assert!(bytes.0[32..].iter().all(|&b| b == 0xff));
}

#[test]
fn new_fits_as_many_groups_as_possible() {
    let mut bytes = Bytes::<{ 3 * 144 - 1 }>::new();
    let table = HashTable::<u64>::new(&mut bytes.0).unwrap();
    assert_eq!(table.groups(), 2);
    assert_eq!(table.capacity(), 2 * GROUP_CAPACITY);
}

#[test]
fn clear_removes_everything() {
    let mut bytes = Bytes::<{ 2 * 144 }>::new();
    let mut table = HashTable::<u64>::new(&mut bytes.0).unwrap();
    for key in 0..28 {
        intern(&mut table, key);
    }
    table.clear();
    assert!(table.is_empty());
    assert_eq!(table.iter().count(), 0);
    assert_eq!(table.find(hash(3), |&s| s == 3), None);
    assert_eq!(intern(&mut table, 3), Some(true));
}

#[test]
fn constant_hash_degrades_to_linear_probing() {
    let mut bytes = Bytes::<{ 4 * 144 }>::new();
    let mut table = HashTable::<u64>::new(&mut bytes.0).unwrap();
    for key in 0..table.capacity() as u64 {
        match table.entry(42, |&s| s == key) {
            Entry::Vacant(entry) => entry.insert(key),
            other => panic!("unexpected {other:?} for {key}"),
        }
    }
    for key in 0..table.capacity() as u64 {
        assert_eq!(table.find(42, |&s| s == key), Some(&key));
    }
    assert!(matches!(table.entry(42, |_| false), Entry::Full));
}

#[test]
fn dropping_a_vacant_entry_changes_nothing() {
    let mut bytes = Bytes::<144>::new();
    let mut table = HashTable::<u64>::new(&mut bytes.0).unwrap();
    assert!(matches!(table.entry(hash(1), |_| false), Entry::Vacant(_)));
    assert!(table.is_empty());
    assert_eq!(table.find(hash(1), |_| true), None);
}

#[test]
fn zero_groups_is_always_full() {
    let mut bytes = Bytes::<16>::new();
    let mut table = HashTable::<u64>::new(&mut bytes.0).unwrap();
    assert_eq!(table.capacity(), 0);
    assert!(matches!(table.entry(hash(1), |_| true), Entry::Full));
    assert_eq!(table.find(hash(1), |_| true), None);
    assert_eq!(table.iter().count(), 0);
}

#[test]
fn layout_errors() {
    let mut bytes = Bytes::<{ 2 * 144 + 1 }>::new();
    assert_eq!(
        HashTable::<u64>::new(&mut bytes.0[1..]).unwrap_err(),
        LayoutError::Misaligned
    );
    assert_eq!(
        HashTable::<u64>::from_zeroed(&mut bytes.0[1..]).unwrap_err(),
        LayoutError::Misaligned
    );
    assert_eq!(
        HashTable::<u64>::reopen(&mut bytes.0[..2 * 144 - 1], 2, 0).unwrap_err(),
        LayoutError::TooSmall
    );
    assert_eq!(
        HashTable::<u64>::reopen(&mut bytes.0, usize::MAX, 0).unwrap_err(),
        LayoutError::TooSmall
    );
    assert_eq!(
        HashTableRef::<u64>::new(&bytes.0, 3).unwrap_err(),
        LayoutError::TooSmall
    );
    // A longer slice is accepted; bytes past the table are unused.
    assert!(HashTable::<u64>::reopen(&mut bytes.0, 2, 0).is_ok());
}

#[test]
fn start_group_stays_in_range() {
    for groups in [1, 2, 3, 7, 113, 1 << 20, usize::MAX >> 8] {
        for h in [0, 1, u64::MAX, u64::MAX >> 7, 1 << 63, hash(groups as u64)] {
            assert!(start_group(h, groups) < groups, "{h:#x} into {groups}");
        }
    }
    assert_eq!(start_group(u64::MAX, 0), 0);
}

#[test]
fn tag_and_start_group_use_disjoint_bits() {
    let top = 0x7f_u64 << GROUP_BITS;
    let low = (1_u64 << GROUP_BITS) - 1;
    for h in [0, 1, hash(1), hash(2), u64::MAX] {
        // The tag depends only on the top 7 bits.
        assert_eq!(tag(h), 0x80 | u8::try_from(h >> GROUP_BITS).unwrap());
        assert_eq!(tag(h), tag(h & top));
        // The start group depends only on the low 57 bits.
        for groups in [1, 7, 113, 1 << 20] {
            assert_eq!(start_group(h, groups), start_group(h & low, groups));
        }
    }
}
