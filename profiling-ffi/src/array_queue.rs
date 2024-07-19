// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use ddcommon_ffi::Error;
use std::ffi::c_void;

#[repr(C)]
pub struct ArrayQueue {
    inner: *mut c_void,
}

impl ArrayQueue {
    pub fn new(inner: *mut c_void) -> Self {
        Self { inner }
    }
}

#[allow(unused)]
#[repr(C)]
pub enum ArrayQueueNewResult {
    Ok(ArrayQueue),
    Err(Error),
}

#[no_mangle]
pub unsafe extern "C" fn array_queue_new(capacity: usize) -> ArrayQueueNewResult {
    let internal_queue: crossbeam_queue::ArrayQueue<*mut c_void> =
        crossbeam_queue::ArrayQueue::new(capacity);
    let internal_queue_ptr = Box::into_raw(Box::new(internal_queue));
    let ffi_queue = ArrayQueue::new(internal_queue_ptr as *mut c_void);
    ArrayQueueNewResult::Ok(ffi_queue)
}

unsafe fn array_queue_ptr_to_inner<'a>(
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

#[no_mangle]
pub unsafe extern "C" fn array_queue_push(
    queue_ptr: *mut ArrayQueue,
    value: *mut c_void,
) -> ArrayQueuePushResult {
    (|| {
        let queue = array_queue_ptr_to_inner(queue_ptr)?;
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

#[no_mangle]
pub unsafe extern "C" fn array_queue_pop(queue_ptr: *mut ArrayQueue) -> ArrayQueuePopResult {
    (|| {
        let queue = array_queue_ptr_to_inner(queue_ptr)?;
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

#[no_mangle]
pub unsafe extern "C" fn array_queue_is_empty(
    queue_ptr: *mut ArrayQueue,
) -> ArrayQueueIsEmptyResult {
    (|| {
        let queue = array_queue_ptr_to_inner(queue_ptr)?;
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

#[no_mangle]
pub unsafe extern "C" fn array_queue_len(queue_ptr: *mut ArrayQueue) -> ArrayQueueLenResult {
    (|| {
        let queue = array_queue_ptr_to_inner(queue_ptr)?;
        anyhow::Ok(queue.len())
    })()
    .context("array_queue_len failed")
    .into()
}
