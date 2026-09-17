// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::platform::PlatformHandle;
use io_lifetimes::{
    views::{SocketlikeView, SocketlikeViewType},
    AsSocketlike,
};
use std::io;
use std::marker::PhantomData;
use std::os::unix::prelude::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::Arc;

impl<T> FromRawFd for PlatformHandle<T> {
    /// Creates PlatformHandle instance from supplied RawFd
    ///
    /// # Safety caller must ensure the RawFd is valid and open, and that the resulting PlatformHandle will
    /// # have exclusive ownership of the file descriptor
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        let inner = Some(Arc::new(OwnedFd::from_raw_fd(fd)));
        Self {
            fd,
            inner,
            phantom: PhantomData,
        }
    }
}

impl<T> From<T> for PlatformHandle<T>
where
    T: IntoRawFd,
{
    fn from(src: T) -> Self {
        unsafe { PlatformHandle::from_raw_fd(src.into_raw_fd()) }
    }
}

impl<T> AsRawFd for PlatformHandle<T> {
    fn as_raw_fd(&self) -> RawFd {
        match &self.inner {
            Some(f) => f.as_raw_fd(),
            None => self.fd,
        }
    }
}

impl<T> PlatformHandle<T>
where
    T: SocketlikeViewType,
{
    pub fn as_socketlike_view(&self) -> io::Result<SocketlikeView<'_, T>> {
        Ok(self.as_owned_fd()?.as_socketlike_view())
    }
}
