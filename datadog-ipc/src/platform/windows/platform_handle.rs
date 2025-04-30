// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::platform::platform_handle::PlatformHandle;
use serde::{Deserialize, Deserializer, Serializer};
use std::marker::PhantomData;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
use std::sync::Arc;

impl<T> FromRawHandle for PlatformHandle<T> {
    /// Creates PlatformHandle instance from supplied RawFd
    ///
    /// # Safety caller must ensure the RawFd is valid and open, and that the resulting PlatformHandle will
    /// # have exclusive ownership of the file descriptor
    unsafe fn from_raw_handle(handle: RawHandle) -> Self {
        let inner = Some(Arc::new(OwnedHandle::from_raw_handle(handle)));
        Self {
            fd: handle,
            inner,
            phantom: PhantomData,
        }
    }
}

impl<T> From<T> for PlatformHandle<T>
where
    T: IntoRawHandle,
{
    fn from(src: T) -> Self {
        unsafe { PlatformHandle::from_raw_handle(src.into_raw_handle()) }
    }
}

impl<T> AsRawHandle for PlatformHandle<T> {
    fn as_raw_handle(&self) -> RawHandle {
        match &self.inner {
            Some(f) => f.as_raw_handle(),
            None => self.fd,
        }
    }
}

pub fn serialize_rawhandle<S>(handle: &RawHandle, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_u64(*handle as u64)
}

pub fn deserialize_rawhandle<'de, D>(deserializer: D) -> Result<RawHandle, D::Error>
where
    D: Deserializer<'de>,
{
    let result: u64 = Deserialize::deserialize(deserializer)?;
    Ok(result as RawHandle)
}

unsafe impl<T> Sync for PlatformHandle<T> {}
unsafe impl<T> Send for PlatformHandle<T> {}
