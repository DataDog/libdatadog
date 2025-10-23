// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::*;
use datadog_profiling::collections::string_table::wordpress_test_data::WORDPRESS_STRINGS;

/// This version is the one we used before having datadog-alloc.
#[allow(unused)]
mod old_version {
    use datadog_profiling::collections::identifiable::{FxIndexSet, Id, InternalStringId};
    pub struct StringTable {
        strings: FxIndexSet<Box<str>>,
    }

    impl StringTable {
        pub fn new() -> Self {
            let mut strings = FxIndexSet::<Box<str>>::default();
            strings.insert("".into());
            Self { strings }
        }

        pub fn intern(&mut self, item: &str) -> InternalStringId {
            // For performance, delay converting the [&str] to a [String] until
            // after it has been determined to not exist in the set. This avoids
            // temporary allocations.
            let index = match self.strings.get_index_of(item) {
                Some(index) => index,
                None => {
                    let (index, _inserted) = self.strings.insert_full(item.into());
                    debug_assert!(_inserted);
                    index
                }
            };
            InternalStringId::from_offset(index)
        }

        #[inline]
        #[allow(clippy::len_without_is_empty)]
        pub fn len(&self) -> usize {
            self.strings.len()
        }
    }
}

// To benchmark a different implementation, import a different one.
use datadog_profiling::collections::string_table::StringTable;
// use old_version::StringTable;

pub fn small_wordpress_profile(c: &mut Criterion) {
    c.bench_function("benching string interning on wordpress profile", |b| {
        b.iter(|| {
            let mut table = StringTable::new();
            let n_strings = WORDPRESS_STRINGS.len();
            for string in WORDPRESS_STRINGS {
                black_box(table.intern(string));
            }
            assert_eq!(n_strings, table.len());

            // re-insert, should nothing should be inserted.
            for string in WORDPRESS_STRINGS {
                black_box(table.intern(string));
            }
            assert_eq!(n_strings, table.len())
        })
    });
}

criterion_group!(benches, small_wordpress_profile);
