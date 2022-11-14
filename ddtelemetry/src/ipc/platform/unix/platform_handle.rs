// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{
    io,
    marker::PhantomData,
    os::unix::prelude::{AsRawFd, FromRawFd, IntoRawFd, RawFd},
    sync::Arc,
};

use io_lifetimes::{
    views::{FilelikeView, FilelikeViewType, SocketlikeView, SocketlikeViewType},
    AsFilelike, AsSocketlike, OwnedFd,
};
use serde::{Deserialize, Serialize};

use crate::ipc::handles::TransferHandles;

/// PlatformHandle contains a valid reference counted FileDescriptor and associated Type information
/// allowing safe transfer and sharing of file handles across processes, and threads
#[derive(Serialize, Deserialize, Debug)]
#[repr(C)]
pub struct PlatformHandle<T> {
    fd: RawFd, // Just an fd number to be used as reference e.g. when serializing, not for accessing actual fd
    #[serde(skip)]
    inner: Option<Arc<OwnedFd>>,
    phantom: PhantomData<T>,
}

impl<T> Default for PlatformHandle<T> {
    fn default() -> Self {
        Self {
            fd: -1,
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
    fn as_owned_fd(&self) -> io::Result<&Arc<OwnedFd>> {
        match &self.inner {
            Some(fd) => Ok(fd),
            None => Err(io::Error::new(
                io::ErrorKind::Other,
                "attempting to unwrap FD from invalid handle".to_string(),
            )),
        }
    }
}

impl<T> PlatformHandle<T> {
    pub fn into_instance(self) -> Result<T, io::Error>
    where
        T: From<OwnedFd>,
    {
        Ok(self.into_owned_handle()?.into())
    }

    pub fn into_owned_handle(self) -> Result<OwnedFd, io::Error> {
        let shared_handle = match self.inner {
            Some(shared_handle) => shared_handle,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "attempting to unwrap FD from invalid handle".to_string(),
                ))
            }
        };

        Arc::try_unwrap(shared_handle).map_err(|_| {
            io::Error::new(
                io::ErrorKind::Other,
                "attempting to unwrap FD from shared platform handle".to_string(),
            )
        })
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

    /// OwnedFd innertype is safe to instantiate via into_instance
    pub fn to_untyped(self) -> PlatformHandle<OwnedFd> {
        unsafe { self.to_any_type() }
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

impl<T> PlatformHandle<T>
where
    T: SocketlikeViewType,
{
    pub fn as_socketlike_view(&self) -> io::Result<SocketlikeView<T>> {
        Ok(self.as_owned_fd()?.as_socketlike_view())
    }
}

impl<T> FromRawFd for PlatformHandle<T> {
    /// Creates PlatformHandle instance from supplied RawFd
    ///
    /// # Safety caller must ensure the RawFd is valid and open, and that the resulting PlatformHandle will
    /// # have exclusive ownership of the file descriptor
    ///
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

impl<T> TransferHandles for PlatformHandle<T> {
    fn move_handles<Transport: crate::ipc::handles::HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        transport.move_handle(self.clone())
    }

    fn receive_handles<Transport: crate::ipc::handles::HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        let received_handle = transport.provide_handle(self)?;
        self.inner = received_handle.inner;
        Ok(())
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
