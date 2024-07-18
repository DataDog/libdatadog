use std::ffi::c_void;

#[derive(Debug)]
#[allow(unused)]
pub struct ArrayQueue {
    inner: crossbeam_queue::ArrayQueue<*mut c_void>,
}

impl ArrayQueue {
    #[allow(unused)]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: crossbeam_queue::ArrayQueue::new(capacity),
        }
    }

    #[allow(unused)]
    pub fn push(&self, value: *mut c_void) -> Result<(), *mut c_void> {
        self.inner.push(value)
    }

    #[allow(unused)]
    pub fn pop(&self) -> Option<*mut c_void> {
        self.inner.pop()
    }

    #[allow(unused)]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }
}
