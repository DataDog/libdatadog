// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Checks that normalizing an already-normalized span doesn't allocate.

use libdd_trace_normalization::normalizer_v1;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

#[path = "../src/test_types.rs"]
mod test_types;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

/// Counts the allocations of the current thread.
struct CountingAllocator;

// SAFETY: it forwards every call to `System`.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|n| n.set(n.get() + 1));
        // SAFETY: the caller upholds the contract of `GlobalAlloc::alloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds the contract of `GlobalAlloc::dealloc`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.with(|n| n.set(n.get() + 1));
        // SAFETY: the caller upholds the contract of `GlobalAlloc::realloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    f();
    ALLOCATIONS.with(Cell::get) - before
}

#[test]
#[cfg_attr(miri, ignore)]
fn normalized_span_does_not_allocate() {
    let mut span: test_types::TestSpan = test_types::normalized_span();
    let n = allocations(|| normalizer_v1::normalize_span(&mut span, "python").unwrap());
    assert_eq!(n, 0);

    // A number as status code doesn't allocate either.
    span.attrs
        .insert("http.status_code".to_owned(), test_types::Value::Int(200));
    let n = allocations(|| normalizer_v1::normalize_span(&mut span, "python").unwrap());
    assert_eq!(n, 0);
}

#[test]
#[cfg_attr(miri, ignore)]
fn normalized_chunk_does_not_allocate() {
    let mut chunk: test_types::TestChunk = test_types::TestChunk {
        trace_id: 1,
        spans: vec![test_types::normalized_span(), test_types::normalized_span()],
        ..Default::default()
    };
    chunk.spans[1].span_id += 1;
    let opts = normalizer_v1::NormalizeOptions {
        lang: "python",
        component2name: false,
    };
    let mut seen = std::collections::HashSet::new();
    // The first call allocates the set, which later calls reuse.
    normalizer_v1::count_duplicate_span_ids(&chunk, &mut seen, &mut ());
    let n = allocations(|| {
        normalizer_v1::normalize_chunk_with(&mut chunk, &opts, &mut ()).unwrap();
        normalizer_v1::count_duplicate_span_ids(&chunk, &mut seen, &mut ());
    });
    assert_eq!(n, 0);
}

#[test]
#[cfg_attr(miri, ignore)]
fn copies_clone_the_stored_text() {
    // With a text that is cheap to clone, the empty-resource fallback and `component2name` clone
    // the stored name and component (`Span::name_text`, `Span::component_text`).
    let mut span: test_types::TestSpan<Arc<str>> = test_types::normalized_span();
    span.resource = Arc::from("");
    let opts = normalizer_v1::NormalizeOptions {
        lang: "python",
        component2name: true,
    };
    let n = allocations(|| normalizer_v1::normalize_span_with(&mut span, &opts, &mut ()).unwrap());
    assert_eq!(n, 0);
    assert_eq!((&*span.name, &*span.resource), ("django", "django"));
    assert!(Arc::ptr_eq(&span.name, span.component.as_ref().unwrap()));
    assert!(Arc::ptr_eq(&span.name, &span.resource));
}
