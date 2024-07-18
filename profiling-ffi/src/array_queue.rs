use std::ffi::c_void;
use std::ptr::NonNull;

#[allow(dead_code)]
pub struct ArrayQueue {
    inner: Box<crossbeam_queue::ArrayQueue<*mut c_void>>,
}

#[no_mangle]
pub extern "C" fn array_queue_new(out_handle: NonNull<Box<ArrayQueue>>, capacity: usize) -> bool {
    let queue = Box::new(ArrayQueue {
        inner: Box::new(crossbeam_queue::ArrayQueue::new(capacity)),
    });
    unsafe {
        out_handle.as_ptr().write(queue);
    }
    true
}

// #[no_mangle]
// pub extern "C" fn queue_new(capacity: usize) -> *mut ArrayQueueWrapper {
//     let queue = Box::new(ArrayQueue::new(capacity));
//     let wrapper = Box::new(ArrayQueueWrapper {
//         inner: Box::into_raw(queue),
//     });
//     Box::into_raw(wrapper)
// }

// #[no_mangle]
// pub extern "C" fn queue_drop(queue: *mut ArrayQueueWrapper) {
//     if !queue.is_null() {
//         unsafe { drop(Box::from_raw(queue)) };
//     }
// }

// // crossbeam_queue::ArrayQueue also implements force_push, which
// #[no_mangle]
// pub extern "C" fn queue_push(queue: *mut ArrayQueueWrapper, value: *mut c_void) -> bool {
//     let queue = unsafe { &(*queue) };
//     let inner = unsafe { &*queue.inner };
//     inner.push(value).is_ok()
// }

// #[no_mangle]
// pub extern "C" fn queue_pop(queue: *mut ArrayQueueWrapper) -> *mut c_void {
//     let queue = unsafe { &(*queue) };
//     let inner = unsafe { &*queue.inner };
//     inner.pop().unwrap_or(std::ptr::null_mut())
// }

// #[no_mangle]
// pub extern "C" fn queue_capacity(queue: *mut ArrayQueueWrapper) -> usize {
//     let queue = unsafe { &(*queue) };
//     let inner = unsafe { &*queue.inner };
//     inner.capacity()
// }

// #[no_mangle]
// pub extern "C" fn queue_len(queue: *mut ArrayQueueWrapper) -> usize {
//     let queue = unsafe { &(*queue) };
//     let inner = unsafe { &*queue.inner };
//     inner.len()
// }

// #[no_mangle]
// pub extern "C" fn queue_is_empty(queue: *mut ArrayQueueWrapper) -> bool {
//     let queue = unsafe { &(*queue) };
//     let inner = unsafe { &*queue.inner };
//     inner.is_empty()
// }
