// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::Error;
use anyhow::Context;
use std::{ffi::c_void, ptr::NonNull};

#[derive(Debug)]
#[repr(C)]
// A simple wrapper around crossbeam_queue::ArrayQueue<*mut c_void>, which is a lock free
// bounded multi-producer and multi-consumer (MPMC) queue.
pub struct ArrayQueue {
    // The actual type here should be NonNull<crossbeam_queue::ArrayQueue<*mut c_void>>.
    // However, cbindgen does not use the module name crossbeam_queue to generate the C header, and
    // above will be generated into ArrayQueue<*void>. So we use NonNull<ArrayQueue> here and cast
    // it to the correct type in the FFI functions. Also, we use NonNull instead of Box to avoid
    // getting into trouble with the drop implementation.
    inner: NonNull<ArrayQueue>,
    item_delete_fn: unsafe extern "C" fn(*mut c_void) -> c_void,
}

unsafe impl Sync for ArrayQueue {}
unsafe impl Send for ArrayQueue {}

impl ArrayQueue {
    pub fn new(
        capacity: usize,
        item_delete_fn: Option<unsafe extern "C" fn(*mut c_void) -> c_void>,
    ) -> anyhow::Result<Self, anyhow::Error> {
        anyhow::ensure!(capacity > 0, "capacity must be greater than 0");
        let item_delete_fn = item_delete_fn.context("item_delete_fn must be non-null")?;

        let internal_queue: crossbeam_queue::ArrayQueue<*mut c_void> =
            crossbeam_queue::ArrayQueue::new(capacity);
        // # Safety: internal_queue must be non-null.
        // If the memory allocation had failed, the program would panic.
        let inner = NonNull::new(Box::into_raw(Box::new(internal_queue)) as *mut ArrayQueue)
            .context("nullptr passed to NonNull, failed to create internal_queue")?;
        Ok(Self {
            inner,
            item_delete_fn,
        })
    }
}

impl<'a> ArrayQueue {
    pub fn as_inner_ref(
        queue: &'a ArrayQueue,
    ) -> anyhow::Result<&'a crossbeam_queue::ArrayQueue<*mut c_void>> {
        // # Safety: the inner points to a valid memory location which is a
        // crossbeam_queue::ArrayQueue<*mut c_void>.
        Ok(unsafe { &*(queue.inner.as_ptr() as *const crossbeam_queue::ArrayQueue<*mut c_void>) })
    }
}

impl Drop for ArrayQueue {
    fn drop(&mut self) {
        // # Safety: the inner pointer is not null and points to a valid memory location
        // holding an crossbeam::ArrayQueue<*mut c_void> created via ArrayQueue::new.
        let queue = unsafe {
            Box::from_raw(self.inner.as_ptr() as *mut crossbeam_queue::ArrayQueue<*mut c_void>)
        };
        while let Some(item) = queue.pop() {
            // # Safety: the item is a valid memory location that can be deallocated by the
            // item_delete_fn.
            unsafe {
                (self.item_delete_fn)(item);
            }
        }
    }
}

#[allow(unused)]
#[repr(C)]
pub enum ArrayQueueNewResult {
    Ok(NonNull<ArrayQueue>),
    Err(Error),
}

/// Creates a new ArrayQueue with the given capacity and item_delete_fn.
/// The item_delete_fn is called when an item is dropped from the queue.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_ArrayQueue_new(
    capacity: usize,
    item_delete_fn: Option<unsafe extern "C" fn(*mut c_void) -> c_void>,
) -> ArrayQueueNewResult {
    match ArrayQueue::new(capacity, item_delete_fn) {
        Ok(queue) => ArrayQueueNewResult::Ok(
            // # Safety: ptr is not null and points to a valid memory location holding an
            // ArrayQueue
            unsafe { NonNull::new_unchecked(Box::into_raw(Box::new(queue))) },
        ),
        Err(err) => ArrayQueueNewResult::Err(err.into()),
    }
}

/// Drops the ArrayQueue.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by ArrayQueue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_drop(queue: *mut ArrayQueue) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    if !queue.is_null() {
        drop(Box::from_raw(queue));
    }
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

/// Pushes an item into the ArrayQueue. It returns the given value if the queue is full.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by ArrayQueue_new. The value
/// is null or points to a valid memory location that can be deallocated by the item_delete_fn.
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_push(
    queue_ptr: &ArrayQueue,
    value: *mut c_void,
) -> ArrayQueuePushResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.push(value))
    })()
    .context("ArrayQueue_push failed")
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
/// The pointer is null or points to a valid memory location allocated by ArrayQueue_new. The value
/// is null or points to a valid memory location that can be deallocated by the item_delete_fn.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_ArrayQueue_force_push(
    queue_ptr: &ArrayQueue,
    value: *mut c_void,
) -> ArrayQueuePushResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.force_push(value))
    })()
    .context("ArrayQueue_force_push failed")
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
/// The pointer is null or points to a valid memory location allocated by ArrayQueue_new.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_pop(queue_ptr: &ArrayQueue) -> ArrayQueuePopResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.pop())
    })()
    .context("ArrayQueue_pop failed")
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
/// The pointer is null or points to a valid memory location allocated by ArrayQueue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_is_empty(queue_ptr: &ArrayQueue) -> ArrayQueueBoolResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.is_empty())
    })()
    .context("ArrayQueue_is_empty failed")
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
/// The pointer is null or points to a valid memory location allocated by ArrayQueue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_len(queue_ptr: &ArrayQueue) -> ArrayQueueUsizeResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.len())
    })()
    .context("ArrayQueue_len failed")
    .into()
}

/// Returns true if the underlying queue is full.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by ArrayQueue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_is_full(queue_ptr: &ArrayQueue) -> ArrayQueueBoolResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.is_full())
    })()
    .context("ArrayQueue_is_full failed")
    .into()
}

/// Returns the capacity of the ArrayQueue.
/// # Safety
/// The pointer is null or points to a valid memory location allocated by ArrayQueue_new.
#[no_mangle]
pub unsafe extern "C" fn ddog_ArrayQueue_capacity(queue_ptr: &ArrayQueue) -> ArrayQueueUsizeResult {
    (|| {
        let queue = ArrayQueue::as_inner_ref(queue_ptr)?;
        anyhow::Ok(queue.capacity())
    })()
    .context("ArrayQueue_capacity failed")
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bolero::TypeGenerator;
    use std::sync::atomic::{AtomicUsize, Ordering};

    unsafe extern "C" fn drop_item(item: *mut c_void) -> c_void {
        _ = Box::from_raw(item as *mut i32);
        std::mem::zeroed()
    }

    #[test]
    fn test_new_drop() {
        let queue_new_result = ddog_ArrayQueue_new(1, Some(drop_item));
        assert!(matches!(queue_new_result, ArrayQueueNewResult::Ok(_)));
        let queue_ptr = match queue_new_result {
            ArrayQueueNewResult::Ok(ptr) => ptr.as_ptr(),
            _ => std::ptr::null_mut(),
        };
        let item = Box::new(1i32);
        let item_ptr = Box::into_raw(item);
        let item2 = Box::new(2i32);
        let item2_ptr = Box::into_raw(item2);
        let item3 = Box::new(3i32);
        let item3_ptr = Box::into_raw(item3);
        unsafe {
            let queue = &*queue_ptr;
            let result = ddog_ArrayQueue_push(queue, item_ptr as *mut c_void);
            assert!(matches!(result, ArrayQueuePushResult::Ok));
            let result = ddog_ArrayQueue_push(queue, item2_ptr as *mut c_void);
            assert!(
                matches!(result, ArrayQueuePushResult::Full(ptr) if ptr == item2_ptr as *mut c_void)
            );
            let result = ddog_ArrayQueue_pop(queue);
            assert!(
                matches!(result, ArrayQueuePopResult::Ok(ptr) if ptr == item_ptr as *mut c_void)
            );
            let item_ptr = match result {
                ArrayQueuePopResult::Ok(ptr) => ptr,
                _ => std::ptr::null_mut(),
            };
            drop(Box::from_raw(item_ptr as *mut i32));
            let result = ddog_ArrayQueue_push(queue, item3_ptr as *mut c_void);
            assert!(matches!(result, ArrayQueuePushResult::Ok));
            ddog_ArrayQueue_drop(queue_ptr);
            drop(Box::from_raw(item2_ptr));
        }
    }

    #[test]
    fn test_capacity_zero() {
        let queue_new_result = ddog_ArrayQueue_new(0, Some(drop_item));
        assert!(matches!(queue_new_result,
                ArrayQueueNewResult::Err(err) if err == Error::from("capacity must be greater than 0")));
    }

    #[test]
    fn test_none_delete_fn() {
        let queue_new_result = ddog_ArrayQueue_new(1, None);
        assert!(matches!(queue_new_result, ArrayQueueNewResult::Err(err)
            if err == Error::from("item_delete_fn must be non-null")));
    }

    #[derive(Debug, TypeGenerator)]
    enum Operation {
        Push,
        ForcePush,
        Pop,
    }

    fn process_ops(ops: &[Operation], queue: &ArrayQueue, cnt: &AtomicUsize) {
        for op in ops {
            match op {
                Operation::Push => {
                    let item = Box::new(1i32);
                    let item_ptr = Box::into_raw(item);
                    let result = unsafe { ddog_ArrayQueue_push(queue, item_ptr as *mut c_void) };
                    match result {
                        ArrayQueuePushResult::Ok => {
                            cnt.fetch_add(1, Ordering::SeqCst);
                        }
                        ArrayQueuePushResult::Full(ptr) => {
                            assert_eq!(ptr, item_ptr as *mut c_void);
                            unsafe {
                                drop(Box::from_raw(ptr as *mut i32));
                            }
                        }
                        ArrayQueuePushResult::Err(_) => {
                            panic!("push failed");
                        }
                    }
                }
                Operation::ForcePush => {
                    let item = Box::new(2i32);
                    let item_ptr = Box::into_raw(item);
                    let result =
                        unsafe { ddog_ArrayQueue_force_push(queue, item_ptr as *mut c_void) };
                    match result {
                        ArrayQueuePushResult::Ok => {
                            cnt.fetch_add(1, Ordering::SeqCst);
                        }
                        ArrayQueuePushResult::Full(ptr) => unsafe {
                            drop(Box::from_raw(ptr as *mut i32));
                        },
                        ArrayQueuePushResult::Err(_) => {
                            panic!("force_push failed");
                        }
                    }
                }
                Operation::Pop => {
                    let result = unsafe { ddog_ArrayQueue_pop(queue) };
                    match result {
                        ArrayQueuePopResult::Ok(ptr) => {
                            cnt.fetch_sub(1, Ordering::SeqCst);
                            unsafe {
                                drop(Box::from_raw(ptr as *mut i32));
                            }
                        }
                        ArrayQueuePopResult::Empty => {
                            // No guarantee that cnt is 0, but would likely be.
                        }
                        ArrayQueuePopResult::Err(_) => {
                            panic!("pop failed");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn fuzz_with_threads() {
        let capacity_gen = 1..=32usize;
        let ops_gen = Vec::<Operation>::produce();

        bolero::check!()
            .with_generator((capacity_gen, ops_gen, ops_gen, ops_gen, ops_gen))
            .for_each(|(capacity, ops1, ops2, ops3, ops4)| {
                let queue_new_result = ddog_ArrayQueue_new(*capacity, Some(drop_item));
                assert!(matches!(queue_new_result, ArrayQueueNewResult::Ok(_)));
                let queue_ptr = match queue_new_result {
                    ArrayQueueNewResult::Ok(ptr) => ptr.as_ptr(),
                    _ => std::ptr::null_mut(),
                };
                let queue = unsafe { &*queue_ptr };

                let cnt = AtomicUsize::new(0);

                std::thread::scope(|s| {
                    s.spawn(|| process_ops(ops1, queue, &cnt));
                    s.spawn(|| process_ops(ops2, queue, &cnt));
                    s.spawn(|| process_ops(ops3, queue, &cnt));
                    s.spawn(|| process_ops(ops4, queue, &cnt));
                });

                // Check the length
                let result = unsafe { ddog_ArrayQueue_len(queue) };
                match result {
                    ArrayQueueUsizeResult::Ok(len) => {
                        assert_eq!(len, cnt.load(Ordering::SeqCst));
                    }
                    ArrayQueueUsizeResult::Err(_) => {
                        panic!("len failed");
                    }
                }

                unsafe {
                    ddog_ArrayQueue_drop(queue_ptr);
                }
            })
    }
}
