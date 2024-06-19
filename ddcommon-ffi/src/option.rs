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
