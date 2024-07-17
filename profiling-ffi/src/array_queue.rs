use crossbeam_queue::ArrayQueue as CrossbeamArrayQueue;
use std::os::raw::c_void;

#[repr(C)]
pub struct ArrayQueue {
    inner: *mut CrossbeamArrayQueue<*mut c_void>,
}

#[no_mangle]
pub extern "C" fn queue_new(capacity: usize) -> *mut ArrayQueue {
    let queue = Box::new(CrossbeamArrayQueue::new(capacity));
    let wrapper = Box::new(ArrayQueue {
        inner: Box::into_raw(queue),
    });
    Box::into_raw(wrapper)
}

#[no_mangle]
pub extern "C" fn queue_drop(queue: *mut ArrayQueue) {
    if !queue.is_null() {
        unsafe { drop(Box::from_raw(queue)) };
    }
}

// crossbeam_queue::ArrayQueue also implements force_push, which
#[no_mangle]
pub extern "C" fn queue_push(queue: *mut ArrayQueue, value: *mut c_void) -> bool {
    let queue = unsafe { &(*queue) };
    let inner = unsafe { &*queue.inner };
    inner.push(value).is_ok()
}

#[no_mangle]
pub extern "C" fn queue_pop(queue: *mut ArrayQueue) -> *mut c_void {
    let queue = unsafe { &(*queue) };
    let inner = unsafe { &*queue.inner };
    inner.pop().unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn queue_capacity(queue: *mut ArrayQueue) -> usize {
    let queue = unsafe { &(*queue) };
    let inner = unsafe { &*queue.inner };
    inner.capacity()
}

#[no_mangle]
pub extern "C" fn queue_len(queue: *mut ArrayQueue) -> usize {
    let queue = unsafe { &(*queue) };
    let inner = unsafe { &*queue.inner };
    inner.len()
}

#[no_mangle]
pub extern "C" fn queue_is_empty(queue: *mut ArrayQueue) -> bool {
    let queue = unsafe { &(*queue) };
    let inner = unsafe { &*queue.inner };
    inner.is_empty()
}
