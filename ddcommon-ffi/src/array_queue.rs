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
        inner: *mut c_void,
        item_delete_fn: unsafe extern "C" fn(*mut c_void) -> c_void,
    ) -> Self {
        Self {
            inner,
            item_delete_fn,
        }
    }
}

impl Drop for ArrayQueue {
    fn drop(&mut self) {
        unsafe {
            let queue = self.inner as *mut crossbeam_queue::ArrayQueue<*mut c_void>;
            while let Some(item) = (*queue).pop() {
                (self.item_delete_fn)(item);
            }
            drop(Box::from_raw(queue));
        }
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
pub extern "C" fn ddog_array_queue_new(
    capacity: usize,
    item_delete_fn: unsafe extern "C" fn(*mut c_void) -> c_void,
) -> ArrayQueueNewResult {
    let internal_queue: crossbeam_queue::ArrayQueue<*mut c_void> =
        crossbeam_queue::ArrayQueue::new(capacity);
    let internal_queue_ptr = Box::into_raw(Box::new(internal_queue));
    let ffi_queue = ArrayQueue::new(internal_queue_ptr as *mut c_void, item_delete_fn);
    ArrayQueueNewResult::Ok(ffi_queue)
}

/// Converts a *mut ArrayQueue to a &mut crossbeam_queue::ArrayQueue<*mut c_void>.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new.
unsafe fn ddog_array_queue_ptr_to_inner<'a>(
    queue_ptr: *mut ArrayQueue,
) -> anyhow::Result<&'a mut crossbeam_queue::ArrayQueue<*mut c_void>> {
    match queue_ptr.as_mut() {
        None => anyhow::bail!("queue_ptr is null"),
        Some(queue) => match queue.inner.as_mut() {
            None => anyhow::bail!("queue.inner is null"),
            Some(inner) => {
                Ok(&mut *(inner as *mut c_void as *mut crossbeam_queue::ArrayQueue<*mut c_void>))
            }
        },
    }
}

/// Drops the ArrayQueue.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_array_queue_drop(queue_ptr: *mut ArrayQueue) {
    if !queue_ptr.is_null() {
        drop(Box::from_raw(queue_ptr));
    }
}

#[allow(unused)]
#[repr(C)]
pub enum ArrayQueuePushResult {
    Ok(bool),
    Err(Error),
}

impl From<Result<(), anyhow::Error>> for ArrayQueuePushResult {
    fn from(result: Result<(), anyhow::Error>) -> Self {
        match result {
            Ok(_) => ArrayQueuePushResult::Ok(true),
            Err(err) => ArrayQueuePushResult::Err(err.into()),
        }
    }
}

/// Pushes an item into the ArrayQueue.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new. The value
/// is null or points to a valid memory location that can be deallocated by the item_delete_fn.
#[no_mangle]
pub unsafe extern "C" fn ddog_array_queue_push(
    queue_ptr: *mut ArrayQueue,
    value: *mut c_void,
) -> ArrayQueuePushResult {
    (|| {
        let queue = ddog_array_queue_ptr_to_inner(queue_ptr)?;
        queue
            .push(value)
            .map_err(|_| anyhow::anyhow!("array_queue full"))
    })()
    .context("array_queue_push failed")
    .into()
}

#[allow(unused)]
#[repr(C)]
pub enum ArrayQueuePopResult {
    Ok(*mut c_void),
    Err(Error),
}

impl From<anyhow::Result<*mut c_void>> for ArrayQueuePopResult {
    fn from(result: anyhow::Result<*mut c_void>) -> Self {
        match result {
            Ok(value) => ArrayQueuePopResult::Ok(value),
            Err(err) => ArrayQueuePopResult::Err(err.into()),
        }
    }
}

/// Pops an item from the ArrayQueue.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_array_queue_pop(queue_ptr: *mut ArrayQueue) -> ArrayQueuePopResult {
    (|| {
        let queue = ddog_array_queue_ptr_to_inner(queue_ptr)?;
        queue
            .pop()
            .ok_or_else(|| anyhow::anyhow!("array_queue empty"))
    })()
    .context("array_queue_pop failed")
    .into()
}

#[allow(unused)]
#[repr(C)]
pub enum ArrayQueueIsEmptyResult {
    Ok(bool),
    Err(Error),
}

impl From<anyhow::Result<bool>> for ArrayQueueIsEmptyResult {
    fn from(result: anyhow::Result<bool>) -> Self {
        match result {
            Ok(value) => ArrayQueueIsEmptyResult::Ok(value),
            Err(err) => ArrayQueueIsEmptyResult::Err(err.into()),
        }
    }
}

/// Checks if the ArrayQueue is empty.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_array_queue_is_empty(
    queue_ptr: *mut ArrayQueue,
) -> ArrayQueueIsEmptyResult {
    (|| {
        let queue = ddog_array_queue_ptr_to_inner(queue_ptr)?;
        anyhow::Ok(queue.is_empty())
    })()
    .context("array_queue_is_empty failed")
    .into()
}

#[allow(unused)]
#[repr(C)]
pub enum ArrayQueueLenResult {
    Ok(usize),
    Err(Error),
}

impl From<anyhow::Result<usize>> for ArrayQueueLenResult {
    fn from(result: anyhow::Result<usize>) -> Self {
        match result {
            Ok(value) => ArrayQueueLenResult::Ok(value),
            Err(err) => ArrayQueueLenResult::Err(err.into()),
        }
    }
}

/// Returns the length of the ArrayQueue.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by array_queue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_array_queue_len(queue_ptr: *mut ArrayQueue) -> ArrayQueueLenResult {
    (|| {
        let queue = ddog_array_queue_ptr_to_inner(queue_ptr)?;
        anyhow::Ok(queue.len())
    })()
    .context("array_queue_len failed")
    .into()
}
