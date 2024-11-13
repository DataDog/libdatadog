// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
pub enum Option<T> {
    Some(T),
    None,
}

impl<T> Option<T> {
    pub fn to_std(self) -> std::option::Option<T> {
        self.into()
    }

    pub fn to_std_ref(&self) -> std::option::Option<&T> {
        match self {
            Option::Some(ref s) => Some(s),
            Option::None => None,
        }
    }

    pub fn as_mut(&mut self) -> Option<&mut T> {
        match *self {
            Option::Some(ref mut x) => Option::Some(x),
            Option::None => Option::None,
        }
    }
}

impl<T> From<Option<T>> for std::option::Option<T> {
    fn from(o: Option<T>) -> Self {
        match o {
            Option::Some(s) => Some(s),
            Option::None => None,
        }
    }
}

impl<T> From<std::option::Option<T>> for Option<T> {
    fn from(o: std::option::Option<T>) -> Self {
        match o {
            Some(s) => Option::Some(s),
            None => Option::None,
        }
    }
}

impl<T: Copy> From<&Option<T>> for std::option::Option<T> {
    fn from(o: &Option<T>) -> Self {
        match o {
            Option::Some(s) => Some(*s),
            Option::None => None,
        }
    }
}

#[no_mangle]
pub extern "C" fn ddog_Option_U32_some(v: u32) -> Option<u32> {
    Option::Some(v)
}

#[no_mangle]
pub extern "C" fn ddog_Option_U32_none() -> Option<u32> {
    Option::None
}
