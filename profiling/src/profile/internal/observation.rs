// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::cell::RefCell;

thread_local! {
    static LENGTH: RefCell<Option<usize>> = RefCell::new( None);
}

#[repr(transparent)]
pub struct Observation {
    data: *mut i64,
}

impl From<Vec<i64>> for Observation {
    fn from(mut v: Vec<i64>) -> Self {
        println!("from {:?} {:?}", v, Self::len());
        if let Some(len) = Self::len() {
            assert_eq!(len, v.len(), "Sample observation was the wrong length");
        } else {
            LENGTH.with(|len| *len.borrow_mut() = Some(v.len()));
        }
        let b = v.into_boxed_slice();
        let p = Box::into_raw(b);

        Self { data: p as *mut i64}
    }
}

impl Observation {
    pub fn len() -> Option<usize> {
        LENGTH.with(|len| *len.borrow())
    }

    pub fn iter_mut(&mut self) -> core::slice::IterMut<'_, i64> {
        self.as_mut_ref().iter_mut()
    }

    pub fn as_ref(&self) -> &[i64] {
        unsafe {
            let len: usize = Self::len().expect("LENGTH to exist by the time we use it");
            std::slice::from_raw_parts(self.data, len)
        }
    }

    pub fn as_mut_ref(&mut self) -> &mut [i64] {
        unsafe {
            let len: usize = Self::len().expect("LENGTH to exist by the time we use it");
            std::slice::from_raw_parts_mut(self.data, len)
        }
    }
}
