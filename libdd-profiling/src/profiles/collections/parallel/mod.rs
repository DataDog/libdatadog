// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod set;
mod sharded;
mod slice_set;
mod string_set;

pub use set::*;
pub use sharded::*;
pub use slice_set::*;
pub use string_set::*;

// `Send`/`Sync` for the parallel collections are derived automatically from
// `Set`/`SliceSet` through `RwLock` and `Arc`. Guard against a change (e.g. a
// new field) silently making them non-thread-safe.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ParallelSet<u64, 4>>();
    assert_send_sync::<ParallelSliceSet<u64>>();
    assert_send_sync::<ParallelStringSet>();
};
