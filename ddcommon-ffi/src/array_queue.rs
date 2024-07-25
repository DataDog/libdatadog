// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::Error;
use anyhow::Context;
use std::ffi::c_void;

#[repr(C)]
// A simple wrapper around crossbeam_queue::ArrayQueue<*mut c_void>, which is a lock free
// bounded multi-producer and multi-consumer (MPMC) queue.
pub struct ArrayQueue {
    // The actual type here should be *mut crossbeam_queue::ArrayQueue<*mut c_void>.
    // However, cbindgen does not use the module name crossbeam_queue to generate the C header.
    // So we use *mut c_void here and cast it to the correct type in the FFI functions.
    inner: *mut c_void,
    item_delete_fn: unsafe extern "C" fn(*mut c_void) -> c_void,
}

impl ArrayQueue {
    pub fn new(
        capacity: usize,
        item_delete_fn: unsafe extern "C" fn(*mut c_void) -> c_void,
    ) -> Self {
        let internal_queue: crossbeam_queue::ArrayQueue<*mut c_void> =
            crossbeam_queue::ArrayQueue::new(capacity);
        let inner = Box::into_raw(Box::new(internal_queue)) as *mut c_void;
        Self {
            inner,
            item_delete_fn,
        }
    }

    pub fn take(&mut self) -> Option<Box<crossbeam_queue::ArrayQueue<*mut c_void>>> {
        let raw = std::mem::replace(&mut self.inner, std::ptr::null_mut());

        if raw.is_null() {
            None
        } else {
            // # Safety: the raw pointer is not null and points to a valid memory location.
            let queue =
                unsafe { Box::from_raw(raw as *mut crossbeam_queue::ArrayQueue<*mut c_void>) };
            while let Some(item) = queue.pop() {
                // # Safety: the item is a valid memory location that can be deallocated by the
                // item_delete_fn.
                unsafe {
                    (self.item_delete_fn)(item);
                }
            }
            Some(queue)
        }
    }
}

impl<'a> ArrayQueue {
    pub fn as_inner_ref(
        queue: &'a mut ArrayQueue,
    ) -> anyhow::Result<&'a crossbeam_queue::ArrayQueue<*mut c_void>> {
        let inner = queue.inner;
        if inner.is_null() {
            anyhow::bail!("queue.inner is null");
        }
        // # Safety: the inner points to a valid memory location which is a
        // crossbeam_queue::ArrayQueue<*mut c_void>.
        Ok(unsafe { &*(inner as *mut crossbeam_queue::ArrayQueue<*mut c_void>) })
    }
}

impl Drop for ArrayQueue {
    fn drop(&mut self) {
        drop(self.take())
    }
}

#[allow(unused)]
#[repr(C)]
pub enum ArrayQueueNewResult {
    Ok(ArrayQueue),
    Err(Error),
}

/// Creates a new ArrayQueue with the given capacity and item_delete_fn.
/// The item_delete_fn is called when an item is dropped from the queue.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_ArrayQueue_new(
    capacity: usize,
    item_delete_fn: unsafe extern "C" fn(*mut c_void) -> c_void,
) -> ArrayQueueNewResult {
    let ffi_queue = ArrayQueue::new(capacity, item_delete_fn);
    ArrayQueueNewResult::Ok(ffi_queue)
}

/// Drops the ArrayQueue.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_drop(queue: &mut ArrayQueue) {
    drop(queue.take());
}

/// Data structure for the result of the push() and force_push() functions.
/// force_push() replaces the oldest element if the queue is full, while push() returns the given
/// value if the queue is full. For push(), it's redundant to return the value since the caller
/// already has it, but it's returned for consistency with crossbeam API and with force_push().
#[allow(unused)]
#[repr(C)]
pub enum ArrayQueuePushResult {
    Ok,
    Full(*mut c_void),
    Err(Error),
}

impl From<Result<Result<(), *mut c_void>, anyhow::Error>> for ArrayQueuePushResult {
    fn from(result: Result<Result<(), *mut c_void>, anyhow::Error>) -> Self {
        match result {
            Ok(value) => match value {
                Ok(()) => ArrayQueuePushResult::Ok,
                Err(value) => ArrayQueuePushResult::Full(value),
            },
            Err(err) => ArrayQueuePushResult::Err(err.into()),
        }
    }
}

/// Pushes an item into the ArrayQueue.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new. The value
/// is null or points to a valid memory location that can be deallocated by the item_delete_fn.
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_push(
    queue_ptr: &mut ArrayQueue,
    value: *mut c_void,
) -> ArrayQueuePushResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.push(value))
    })()
    .context("array_queue_push failed")
    .into()
}

impl From<Result<Option<*mut c_void>, anyhow::Error>> for ArrayQueuePushResult {
    fn from(result: Result<Option<*mut c_void>, anyhow::Error>) -> Self {
        match result {
            Ok(value) => match value {
                Some(value) => ArrayQueuePushResult::Full(value),
                None => ArrayQueuePushResult::Ok,
            },
            Err(err) => ArrayQueuePushResult::Err(err.into()),
        }
    }
}

/// Pushes an element into the queue, replacing the oldest element if necessary.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new. The value
/// is null or points to a valid memory location that can be deallocated by the item_delete_fn.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_ArrayQueue_force_push(
    queue_ptr: &mut ArrayQueue,
    value: *mut c_void,
) -> ArrayQueuePushResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.force_push(value))
    })()
    .context("array_queue_force_push failed")
    .into()
}

#[allow(unused)]
#[repr(C)]
pub enum ArrayQueuePopResult {
    Ok(*mut c_void),
    Empty,
    Err(Error),
}

impl From<anyhow::Result<Option<*mut c_void>>> for ArrayQueuePopResult {
    fn from(result: anyhow::Result<Option<*mut c_void>>) -> Self {
        match result {
            Ok(value) => match value {
                Some(value) => ArrayQueuePopResult::Ok(value),
                None => ArrayQueuePopResult::Empty,
            },
            Err(err) => ArrayQueuePopResult::Err(err.into()),
        }
    }
}

/// Pops an item from the ArrayQueue.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_pop(queue_ptr: &mut ArrayQueue) -> ArrayQueuePopResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.pop())
    })()
    .context("array_queue_pop failed")
    .into()
}

#[allow(unused)]
#[repr(C)]
pub enum ArrayQueueBoolResult {
    Ok(bool),
    Err(Error),
}

impl From<anyhow::Result<bool>> for ArrayQueueBoolResult {
    fn from(result: anyhow::Result<bool>) -> Self {
        match result {
            Ok(value) => ArrayQueueBoolResult::Ok(value),
            Err(err) => ArrayQueueBoolResult::Err(err.into()),
        }
    }
}

/// Checks if the ArrayQueue is empty.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_is_empty(
    queue_ptr: &mut ArrayQueue,
) -> ArrayQueueBoolResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.is_empty())
    })()
    .context("array_queue_is_empty failed")
    .into()
}

#[allow(unused)]
#[repr(C)]
pub enum ArrayQueueUsizeResult {
    Ok(usize),
    Err(Error),
}

impl From<anyhow::Result<usize>> for ArrayQueueUsizeResult {
    fn from(result: anyhow::Result<usize>) -> Self {
        match result {
            Ok(value) => ArrayQueueUsizeResult::Ok(value),
            Err(err) => ArrayQueueUsizeResult::Err(err.into()),
        }
    }
}

/// Returns the length of the ArrayQueue.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_len(queue_ptr: &mut ArrayQueue) -> ArrayQueueUsizeResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.len())
    })()
    .context("array_queue_len failed")
    .into()
}

/// Returns true if the underlying queue is full.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_is_full(
    queue_ptr: &mut ArrayQueue,
) -> ArrayQueueBoolResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.is_full())
    })()
    .context("array_queue_is_full failed")
    .into()
}

/// Returns the capacity of the ArrayQueue.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_capacity(
    queue_ptr: &mut ArrayQueue,
) -> ArrayQueueUsizeResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.capacity())
    })()
    .context("array_queue_capacity failed")
    .into()
}
