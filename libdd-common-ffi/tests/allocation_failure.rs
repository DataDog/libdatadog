// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

extern crate alloc;

use alloc::vec;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::Cell;
use core::ffi::c_char;
use core::ptr::null_mut;
use libdd_common::tag::Tag;
use libdd_common_ffi::slice::{AsBytes, CharSlice};
use libdd_common_ffi::tags::{PushTagResult, ddog_Vec_Tag_push};
use libdd_common_ffi::{Error, Handle, ToInner, Vec, ddog_Error_drop, ddog_Error_message};
use std::alloc::System;

struct FailingAllocator;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Disabled,
    Count,
    FailOnce,
    FailFrom,
}

#[derive(Clone, Copy, Debug)]
struct State {
    mode: Mode,
    fail_at: usize,
    allocations: usize,
    deallocations: usize,
}

impl State {
    const DISABLED: Self = Self {
        mode: Mode::Disabled,
        fail_at: usize::MAX,
        allocations: 0,
        deallocations: 0,
    };
}

thread_local! {
    static STATE: Cell<State> = const { Cell::new(State::DISABLED) };
}

#[global_allocator]
static ALLOCATOR: FailingAllocator = FailingAllocator;

impl FailingAllocator {
    fn should_fail(&self) -> bool {
        STATE.with(|state| {
            let mut current = state.get();
            if current.mode == Mode::Disabled {
                return false;
            }

            let ordinal = current.allocations;
            current.allocations += 1;
            state.set(current);
            match current.mode {
                Mode::FailOnce => ordinal == current.fail_at,
                Mode::FailFrom => ordinal >= current.fail_at,
                Mode::Disabled | Mode::Count => false,
            }
        })
    }

    fn record_deallocation(&self) {
        STATE.with(|state| {
            let mut current = state.get();
            if current.mode != Mode::Disabled {
                current.deallocations += 1;
                state.set(current);
            }
        });
    }
}

unsafe impl GlobalAlloc for FailingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if self.should_fail() {
            null_mut()
        } else {
            // SAFETY: delegated to the system allocator with the original layout.
            unsafe { System.alloc(layout) }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if self.should_fail() {
            null_mut()
        } else {
            // SAFETY: delegated to the system allocator with the original layout.
            unsafe { System.alloc_zeroed(layout) }
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if self.should_fail() {
            null_mut()
        } else {
            // SAFETY: delegated to the system allocator with the original arguments.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.record_deallocation();
        // SAFETY: delegated to the system allocator with the original arguments.
        unsafe { System.dealloc(ptr, layout) }
    }
}

fn start(mode: Mode, fail_at: usize) {
    // Initialize the thread-local state before allocation failure is enabled.
    STATE.with(|state| {
        state.set(State {
            mode,
            fail_at,
            allocations: 0,
            deallocations: 0,
        });
    });
}

fn snapshot() -> State {
    STATE.with(Cell::get)
}

fn stop() -> State {
    STATE.with(|state| {
        let result = state.get();
        state.set(State::DISABLED);
        result
    })
}

fn full_tag_vec() -> Vec<Tag> {
    let tags = vec![Tag::try_new("existing", "tag").unwrap()];
    assert_eq!(tags.len(), tags.capacity());
    tags.into()
}

fn assert_push_reports_every_allocation_failure(
    make_vec: impl Fn() -> Vec<Tag>,
    key: CharSlice<'static>,
    value: CharSlice<'static>,
) {
    let mut tags = make_vec();
    start(Mode::Count, usize::MAX);
    let baseline = unsafe { ddog_Vec_Tag_push(&mut tags, key, value) };
    let allocation_count = stop().allocations;
    assert!(matches!(baseline, PushTagResult::Ok));
    assert!(allocation_count > 0);
    drop(tags);

    for mode in [Mode::FailOnce, Mode::FailFrom] {
        for fail_at in 0..allocation_count {
            let mut tags = make_vec();
            let original_len = tags.len();
            start(mode, fail_at);
            let result = unsafe { ddog_Vec_Tag_push(&mut tags, key, value) };

            let PushTagResult::Err(mut error) = result else {
                stop();
                panic!("{mode:?} allocation {fail_at} unexpectedly succeeded");
            };
            let before_message = snapshot();
            let message = unsafe { ddog_Error_message(Some(&error)) };
            let message_is_oom = message.as_bytes() == b"memory allocation failed";
            unsafe {
                ddog_Error_drop(Some(&mut error));
                ddog_Error_drop(Some(&mut error));
            }
            let after_clear = stop();

            assert_eq!(tags.len(), original_len);
            assert!(message_is_oom);
            assert_eq!(before_message.allocations, after_clear.allocations);
            assert_eq!(before_message.deallocations, after_clear.deallocations);
            drop(tags);
        }
    }
}

#[test]
fn tag_push_returns_static_oom_for_fail_once_and_fail_from() {
    assert_push_reports_every_allocation_failure(
        Vec::new,
        CharSlice::from("key"),
        CharSlice::from("value"),
    );
    assert_push_reports_every_allocation_failure(
        full_tag_vec,
        CharSlice::from("key"),
        CharSlice::from("value"),
    );

    const INVALID_UTF8: &[u8] = &[b'a', 0xf7];
    let invalid = unsafe { CharSlice::from_raw_parts(INVALID_UTF8.as_ptr().cast::<c_char>(), 2) };
    assert_push_reports_every_allocation_failure(Vec::new, invalid, CharSlice::from("value"));
    assert_push_reports_every_allocation_failure(Vec::new, CharSlice::from("key"), invalid);
}

#[test]
fn empty_handle_paths_do_not_allocate_under_fail_from() {
    let mut null_handle = null_mut::<Handle<u8>>();
    let mut empty = Handle::<u8>::empty();

    start(Mode::FailFrom, 0);
    let null_access = unsafe { null_handle.to_inner_mut() }.unwrap_err();
    let null_take = unsafe { null_handle.take() }.unwrap_err();
    let empty_access = unsafe { empty.to_inner_mut() }.unwrap_err();
    let empty_take = unsafe { empty.take() }.unwrap_err();
    drop(empty);
    let state = stop();

    assert_eq!(null_access, libdd_common_ffi::HandleError::NullPointer);
    assert_eq!(null_take, libdd_common_ffi::HandleError::NullPointer);
    assert_eq!(empty_access, libdd_common_ffi::HandleError::Empty);
    assert_eq!(empty_take, libdd_common_ffi::HandleError::Empty);
    assert_eq!(state.allocations, 0);
    assert_eq!(state.deallocations, 0);
}

#[test]
fn ordinary_error_clear_deallocates_heap_storage() {
    let mut error = Error::from(String::from("heap-backed message"));

    start(Mode::Count, usize::MAX);
    unsafe { ddog_Error_drop(Some(&mut error)) };
    let state = stop();

    assert_eq!(state.allocations, 0);
    assert_eq!(state.deallocations, 1);
}

#[test]
fn error_message_formatting_falls_back_to_static_oom() {
    for mode in [Mode::FailOnce, Mode::FailFrom] {
        start(mode, 0);
        let mut error = Error::from("heap-backed message");
        let before_message = snapshot();
        let message = unsafe { ddog_Error_message(Some(&error)) };
        let message_is_oom = message.as_bytes() == b"memory allocation failed";
        unsafe {
            ddog_Error_drop(Some(&mut error));
            ddog_Error_drop(Some(&mut error));
        }
        let after_clear = stop();

        assert!(message_is_oom);
        assert_eq!(before_message.allocations, after_clear.allocations);
        assert_eq!(before_message.deallocations, after_clear.deallocations);
    }
}

#[test]
fn anyhow_formatting_falls_back_to_static_oom() {
    for mode in [Mode::FailOnce, Mode::FailFrom] {
        let source = anyhow::anyhow!("heap-backed source error");
        start(mode, 0);
        let mut error = Error::from(source);
        let before_message = snapshot();
        let message = unsafe { ddog_Error_message(Some(&error)) };
        let message_is_oom = message.as_bytes() == b"memory allocation failed";
        unsafe { ddog_Error_drop(Some(&mut error)) };
        let after_clear = stop();

        assert!(message_is_oom);
        assert_eq!(before_message.allocations, after_clear.allocations);
        assert_eq!(before_message.deallocations, after_clear.deallocations);
    }
}

#[test]
fn panic_error_formatting_falls_back_to_static_oom() {
    for mode in [Mode::FailOnce, Mode::FailFrom] {
        let payload: Box<dyn core::any::Any + Send> = Box::new("panic message");
        start(mode, 0);
        let mut error = libdd_common_ffi::utils::handle_panic_error(payload, "fallible_function");
        let before_message = snapshot();
        let message = unsafe { ddog_Error_message(Some(&error)) };
        let message_is_oom = message.as_bytes() == b"memory allocation failed";
        unsafe { ddog_Error_drop(Some(&mut error)) };
        let after_clear = stop();

        assert!(message_is_oom);
        assert_eq!(before_message.allocations, after_clear.allocations);
        assert_eq!(before_message.deallocations, after_clear.deallocations);
    }
}
