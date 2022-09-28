// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

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
}

impl<T> From<Option<T>> for std::option::Option<T> {
    fn from(o: Option<T>) -> Self {
        match o {
            Option::Some(s) => Some(s),
            Option::None => None,
        }
    }
}
