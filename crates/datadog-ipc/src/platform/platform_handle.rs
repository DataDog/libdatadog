// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(windows)]
//noinspection RsUnusedImport
use crate::platform::{deserialize_rawhandle, serialize_rawhandle};

use io_lifetimes::views::{FilelikeView, FilelikeViewType};
use io_lifetimes::AsFilelike;
use serde::{Deserialize, Serialize};
use std::{io, marker::PhantomData, sync::Arc};

use crate::handles::TransferHandles;

#[cfg(not(windows))]
type RawFileHandle = std::os::unix::prelude::RawFd;
#[cfg(not(windows))]
pub type OwnedFileHandle = io_lifetimes::OwnedFd;
#[cfg(windows)]
type RawFileHandle = std::os::windows::io::RawHandle;
#[cfg(windows)]
pub type OwnedFileHandle = std::os::windows::io::OwnedHandle;

/// PlatformHandle contains a valid reference counted FileDescriptor and associated Type information
/// allowing safe transfer and sharing of file handles across processes, and threads
#[derive(Serialize, Deserialize, Debug)]
pub struct PlatformHandle<T> {
    #[cfg_attr(
        windows,
        serde(
            deserialize_with = "deserialize_rawhandle",
            serialize_with = "serialize_rawhandle"
        )
    )]
    pub(crate) fd: RawFileHandle, /* Just an fd number to be used as reference e.g. when
                                   * serializing, not for accessing actual fd */
    #[serde(skip)]
    pub(crate) inner: Option<Arc<OwnedFileHandle>>,
    pub(crate) phantom: PhantomData<T>,
}

impl<T> Default for PlatformHandle<T> {
    fn default() -> Self {
        Self {
            fd: -1i64 as RawFileHandle,
            inner: None,
            phantom: Default::default(),
        }
    }
}

impl<T> Clone for PlatformHandle<T> {
    fn clone(&self) -> Self {
        Self {
            fd: self.fd,
            inner: self.inner.clone(),
            phantom: PhantomData,
        }
    }
}

impl<T> PlatformHandle<T> {
    pub(crate) fn as_owned_fd(&self) -> io::Result<&Arc<OwnedFileHandle>> {
        match &self.inner {
            Some(fd) => Ok(fd),
            None => Err(io::Error::other(
                "attempting to unwrap FD from invalid handle",
            )),
        }
    }
}

impl<T> PlatformHandle<T> {
    pub fn into_instance(self) -> Result<T, io::Error>
    where
        T: From<OwnedFileHandle>,
    {
        Ok(self.into_owned_handle()?.into())
    }

    pub fn into_owned_handle(self) -> Result<OwnedFileHandle, io::Error> {
        let shared_handle = match self.inner {
            Some(shared_handle) => shared_handle,
            None => {
                return Err(io::Error::other(
                    "attempting to unwrap FD from invalid handle",
                ))
            }
        };

        Arc::try_unwrap(shared_handle)
            .map_err(|_| io::Error::other("attempting to unwrap FD from shared platform handle"))
    }

    /// casts the associated type
    ///
    /// # Safety
    /// Caller must ensure the  type is compatible with the stored FD
    pub unsafe fn to_any_type<U>(self) -> PlatformHandle<U> {
        PlatformHandle {
            fd: self.fd,
            inner: self.inner,
            phantom: PhantomData,
        }
    }

    /// OwnedFileHandle innertype is safe to instantiate via into_instance
    pub fn to_untyped(self) -> PlatformHandle<OwnedFileHandle> {
        unsafe { self.to_any_type() }
    }
}

impl<T> TransferHandles for PlatformHandle<T> {
    fn move_handles<Transport: crate::handles::HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        transport.move_handle(self.clone())
    }

    fn receive_handles<Transport: crate::handles::HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        let received_handle = transport.provide_handle(self)?;
        self.inner = received_handle.inner;
        Ok(())
    }
}

impl<T> PlatformHandle<T>
where
    T: FilelikeViewType,
{
    pub fn as_filelike_view(&self) -> io::Result<FilelikeView<'_, T>> {
        Ok(self.as_owned_fd()?.as_filelike_view())
    }
}

#[cfg(test)]
mod tests {
    use std::{fs::File, io::Write, thread};

    use super::PlatformHandle;
    macro_rules! assert_file_is_open_for_writing {
        ($file:expr) => {{
            writeln!($file, "test").unwrap();
        }};
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_platform_handles_fd_handling() {
        let mut file = tempfile::tempfile().unwrap();
        assert_file_is_open_for_writing!(file);

        let shared = PlatformHandle::from(file);

        {
            let clone = shared.clone();
            // can't uniquely own a shared instance with multiple owners
            shared.clone().into_owned_handle().unwrap_err();
            clone.into_owned_handle().unwrap_err();
        }
        // once no one is using the instance we can convert it to owned
        let owned = shared.into_owned_handle().unwrap();
        let mut file: File = owned.into();

        assert_file_is_open_for_writing!(file);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_platform_handle_fd_borrowing() {
        let mut file = tempfile::tempfile().unwrap();
        assert_file_is_open_for_writing!(file);

        let shared = PlatformHandle::from(file);
        let mut joins = vec![];
        for _ in 0..100 {
            let shared = shared.clone();
            let th = thread::spawn(move || {
                let mut file = &*shared.as_filelike_view().unwrap();
                assert_file_is_open_for_writing!(file);
                1
            });
            joins.push(th);
        }
        let cnt: i32 = joins.into_iter().map(|j| j.join().unwrap()).sum();
        assert_eq!(100, cnt);

        let mut file = &*shared.as_filelike_view().unwrap();
        assert_file_is_open_for_writing!(file);
    }
}
