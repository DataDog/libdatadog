// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::*;
use datadog_profiling::collections::string_table::{
    wordpress_test_data::WORDPRESS_STRINGS, StringTable,
};

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
