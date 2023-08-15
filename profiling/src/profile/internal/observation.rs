// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::cell::RefCell;

thread_local! {
    static LENGTH: RefCell<Option<usize>> = RefCell::new( None);
}

/// This represents a `Vec<i64>` associated with a sample
///
#[repr(transparent)]
pub struct Observation {
    data: *mut i64,
}

impl From<Vec<i64>> for Observation {
    fn from(v: Vec<i64>) -> Self {
        if let Some(len) = Self::len() {
            assert_eq!(len, v.len(), "Sample observation was the wrong length");
        } else {
            LENGTH.with(|len| *len.borrow_mut() = Some(v.len()));
        }
        let b = v.into_boxed_slice();
        let p = Box::into_raw(b);
        let data = p as *mut i64;
        Self { data }
    }
}

impl std::convert::AsRef<[i64]> for Observation {
    fn as_ref(&self) -> &[i64] {
        unsafe {
            let len: usize = Self::len().expect("LENGTH to exist by the time we use it");
            std::slice::from_raw_parts(self.data, len)
        }
    }
}

impl std::convert::AsMut<[i64]> for Observation {
    fn as_mut(&mut self) -> &mut [i64] {
        unsafe {
            let len: usize = Self::len().expect("LENGTH to exist by the time we use it");
            std::slice::from_raw_parts_mut(self.data, len)
        }
    }
}

impl Observation {
    pub fn iter(&self) -> core::slice::Iter<'_, i64> {
        self.as_ref().iter()
    }

    pub fn iter_mut(&mut self) -> core::slice::IterMut<'_, i64> {
        self.as_mut().iter_mut()
    }

    pub fn len() -> Option<usize> {
        LENGTH.with(|len| *len.borrow())
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        unsafe {
            let r = self.as_mut() as *mut [i64];
            let b = Box::from_raw(r);
            std::mem::drop(b)
        }
    }
}
