// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Contract tests of `ScopedFailingAllocator`, run in-process.
//!
//! Inside a scope, the tests only call the raw allocation functions and store results in locals;
//! assertions (which may allocate) run after the scope ends.

use crate::allocator::{self, Domain, Event, Operation, Policy, Scope};
use std::alloc::{Layout, alloc, alloc_zeroed, dealloc, realloc};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

const LAYOUT: Layout = match Layout::from_size_align(48, 16) {
    Ok(l) => l,
    Err(_) => panic!("invalid layout"),
};

fn layout(size: usize) -> Layout {
    Layout::from_size_align(size, LAYOUT.align()).unwrap()
}

/// Frees `p` if it is not null.
unsafe fn free(p: *mut u8, layout: Layout) {
    if !p.is_null() {
        unsafe { dealloc(p, layout) }
    }
}

fn filled_with(p: *const u8, len: usize, byte: u8) -> bool {
    unsafe { std::slice::from_raw_parts(p, len) }
        .iter()
        .all(|&b| b == byte)
}

pub fn pass_through() {
    allocator::reset();
    unsafe {
        let a = alloc(LAYOUT);
        assert!(!a.is_null());
        a.write_bytes(0xab, LAYOUT.size());

        let z = alloc_zeroed(LAYOUT);
        assert!(!z.is_null());
        assert!(filled_with(z, LAYOUT.size(), 0));

        let r = realloc(a, LAYOUT, 4096);
        assert!(!r.is_null());
        assert!(filled_with(r, LAYOUT.size(), 0xab));

        dealloc(r, layout(4096));
        dealloc(z, LAYOUT);
    }
    assert_eq!(allocator::recorded().unwrap(), vec![]);
    assert_eq!(allocator::injected(), 0);
}

pub fn ordinal_counting() {
    allocator::reset();
    let small = Layout::from_size_align(8, 8).unwrap();
    let (a, b, z, r);
    {
        let _scope = Scope::enter(Policy::Observe);
        unsafe {
            a = alloc(LAYOUT);
            b = alloc(small);
            dealloc(b, small);
            z = alloc_zeroed(LAYOUT);
            r = realloc(a, LAYOUT, 96);
        }
    }
    unsafe {
        free(r, layout(96));
        free(z, LAYOUT);
    }
    let thread = allocator::current_thread_id();
    let got: Vec<_> = allocator::recorded()
        .unwrap()
        .iter()
        .map(|e| (e.ordinal, e.operation, e.size, e.align, e.thread))
        .collect();
    assert_eq!(
        got,
        vec![
            (0, Operation::Alloc, 48, 16, thread),
            (1, Operation::Alloc, 8, 8, thread),
            (2, Operation::AllocZeroed, 48, 16, thread),
            (3, Operation::Realloc, 96, 16, thread),
        ]
    );
    assert!(!a.is_null() && !b.is_null() && !z.is_null() && !r.is_null());
    assert_eq!(allocator::injected(), 0);
}

pub fn fail_once() {
    allocator::reset();
    let mut p = [std::ptr::null_mut(); 4];
    {
        let _scope = Scope::enter(Policy::FailOnceAt(1));
        for slot in &mut p {
            *slot = unsafe { alloc(LAYOUT) };
        }
    }
    let nulls: Vec<bool> = p.iter().map(|x| x.is_null()).collect();
    for x in p {
        unsafe { free(x, LAYOUT) };
    }
    assert_eq!(nulls, vec![false, true, false, false]);
    assert_eq!(allocator::injected(), 1);
    assert_eq!(allocator::recorded().unwrap().len(), 4);
}

pub fn fail_from() {
    allocator::reset();
    let base = unsafe { alloc(LAYOUT) };
    assert!(!base.is_null());
    let (a, b, z, r);
    {
        let _scope = Scope::enter(Policy::FailFrom(1));
        unsafe {
            a = alloc(LAYOUT);
            b = alloc(LAYOUT);
            z = alloc_zeroed(LAYOUT);
            r = realloc(base, LAYOUT, 256);
        }
    }
    let nulls = [a.is_null(), b.is_null(), z.is_null(), r.is_null()];
    unsafe {
        free(a, LAYOUT);
        free(b, LAYOUT);
        free(z, LAYOUT);
        if r.is_null() {
            dealloc(base, LAYOUT);
        } else {
            dealloc(r, layout(256));
        }
    }
    assert_eq!(nulls, [false, true, true, true]);
    assert_eq!(allocator::injected(), 3);
}

pub fn realloc_preserves_block() {
    allocator::reset();
    let p = unsafe { alloc(LAYOUT) };
    assert!(!p.is_null());
    unsafe { p.write_bytes(0x5a, LAYOUT.size()) };
    let r = {
        let _scope = Scope::enter(Policy::FailOnceAt(0));
        unsafe { realloc(p, LAYOUT, 1 << 20) }
    };
    assert!(r.is_null());
    assert!(filled_with(p, LAYOUT.size(), 0x5a));
    // The old block is still owned by the caller and can grow later.
    let r = unsafe { realloc(p, LAYOUT, 4096) };
    assert!(!r.is_null());
    assert!(filled_with(r, LAYOUT.size(), 0x5a));
    unsafe { dealloc(r, layout(4096)) };
    assert_eq!(allocator::injected(), 1);
}

pub fn nested_scopes() {
    allocator::reset();
    let mut p = [std::ptr::null_mut(); 4];
    {
        let _outer = Scope::enter(Policy::FailOnceAt(2));
        p[0] = unsafe { alloc(LAYOUT) };
        {
            // Joins the outer scope: same counter, its own policy is ignored.
            let _inner = Scope::enter(Policy::FailFrom(0));
            p[1] = unsafe { alloc(LAYOUT) };
            p[2] = unsafe { alloc(LAYOUT) };
        }
        p[3] = unsafe { alloc(LAYOUT) };
    }
    let after = unsafe { alloc(LAYOUT) };
    let nulls: Vec<bool> = p.iter().map(|x| x.is_null()).collect();
    for x in p {
        unsafe { free(x, LAYOUT) };
    }
    unsafe { free(after, LAYOUT) };
    assert_eq!(nulls, vec![false, false, true, false]);
    assert!(!after.is_null());
    let ordinals: Vec<u64> = allocator::recorded()
        .unwrap()
        .iter()
        .map(|e| e.ordinal)
        .collect();
    assert_eq!(ordinals, vec![0, 1, 2, 3]);
}

pub fn thread_isolation() {
    static GO: AtomicBool = AtomicBool::new(false);
    static DONE: AtomicBool = AtomicBool::new(false);
    static OTHER: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());

    allocator::reset();
    let other = std::thread::spawn(|| {
        while !GO.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
        OTHER.store(unsafe { alloc(LAYOUT) }, Ordering::Relaxed);
        DONE.store(true, Ordering::Release);
    });
    let mine = {
        let _scope = Scope::enter(Policy::FailFrom(0));
        GO.store(true, Ordering::Release);
        while !DONE.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
        unsafe { alloc(LAYOUT) }
    };
    other.join().unwrap();
    let theirs = OTHER.load(Ordering::Relaxed);
    unsafe { free(theirs, LAYOUT) };
    assert!(mine.is_null(), "the scoped thread's allocation must fail");
    assert!(!theirs.is_null(), "another thread is not in the scope");
    let trace = allocator::recorded().unwrap();
    assert_eq!(trace.len(), 1, "{trace:?}");
    assert_eq!(trace[0].thread, allocator::current_thread_id());
}

pub fn prefix_divergence() {
    allocator::reset();
    let expected = Event {
        ordinal: 0,
        domain: Domain::Global,
        operation: Operation::Alloc,
        size: 32,
        align: 16,
        thread: 0,
    };
    allocator::set_expected(&[
        expected,
        Event {
            ordinal: 1,
            ..expected
        },
    ])
    .unwrap();
    let (a, b);
    {
        let _scope = Scope::enter(Policy::FailOnceAt(1));
        // 48 bytes instead of the expected 32: the run diverged, so nothing is injected.
        a = unsafe { alloc(LAYOUT) };
        b = unsafe { alloc(layout(32)) };
    }
    let nulls = [a.is_null(), b.is_null()];
    unsafe {
        free(a, LAYOUT);
        free(b, layout(32));
    }
    assert_eq!(nulls, [false, false]);
    assert_eq!(allocator::diverged_at(), Some(0));
    assert_eq!(allocator::injected(), 0);
}
