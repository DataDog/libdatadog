// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use pin_project::pin_project;
use sendfd::{RecvWithFd, SendWithFd};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::{
    io,
    os::unix::prelude::{AsRawFd, RawFd},
    sync::{Arc, Mutex},
    task::Poll,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::UnixStream,
};

use super::{Channel, ChannelMetadata, MAX_FDS};
use crate::platform::PlatformHandle;

#[derive(Debug)]
#[pin_project]
pub struct AsyncChannel {
    #[pin]
    inner: UnixStream,
    pub metadata: Arc<Mutex<ChannelMetadata>>,
}

impl From<UnixStream> for AsyncChannel {
    fn from(stream: UnixStream) -> Self {
        AsyncChannel {
            inner: stream,
            metadata: Arc::new(Mutex::new(ChannelMetadata::default())),
        }
    }
}

impl TryFrom<Channel> for AsyncChannel {
    type Error = io::Error;

    fn try_from(value: Channel) -> Result<Self, Self::Error> {
        let fd = PlatformHandle::<StdUnixStream>::from(value).into_instance()?;

        fd.set_nonblocking(true)?;
        Ok(AsyncChannel {
            inner: UnixStream::from_std(fd)?,
            metadata: Arc::new(Mutex::new(ChannelMetadata::default())),
        })
    }
}

impl AsyncWrite for AsyncChannel {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let project = self.project();
        #[allow(clippy::unwrap_used)]
        let handles = project.metadata.lock().unwrap().drain_to_send();

        if !handles.is_empty() {
            let fds: Vec<RawFd> = handles.iter().map(AsRawFd::as_raw_fd).collect();
            match project.inner.send_with_fd(buf, &fds) {
                Ok(sent) => Poll::Ready(Ok(sent)),
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    #[allow(clippy::unwrap_used)]
                    project
                        .metadata
                        .lock()
                        .unwrap()
                        .reenqueue_for_sending(handles);
                    project.inner.poll_write_ready(cx).map_ok(|_| 0)
                }
                Err(err) => Poll::Ready(Err(err)),
            }
        } else {
            project.inner.poll_write(cx, buf)
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        self.project().inner.poll_shutdown(cx)
    }
}

impl AsyncRead for AsyncChannel {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let project = self.project();
        let mut fds = [0; MAX_FDS];

        // Safety: this implementation is based on Tokio async read implementation,
        // it is performing an UB operation by using uninitiallized memory - although in practice
        // its somewhat defined there are still some unknowns WRT to future behaviors
        // TODO: make sure this optimization is really needed - once BenchPlatform is connected to
        // libdatadog benchmark unfilled_mut vs initialize_unfilled - and if the difference
        // is negligible - then lets switch to implementation that doesn't use UB.
        unsafe {
            let b = &mut *(buf.unfilled_mut() as *mut [std::mem::MaybeUninit<u8>] as *mut [u8]);
            loop {
                break match project.inner.recv_with_fd(b, &mut fds) {
                    Ok((bytes_received, descriptors_received)) => {
                        #[allow(clippy::unwrap_used)]
                        project
                            .metadata
                            .lock()
                            .unwrap()
                            .receive_fds(&fds[..descriptors_received]);

                        buf.assume_init(bytes_received);
                        buf.advance(bytes_received);

                        Poll::Ready(Ok(()))
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        match project.inner.poll_read_ready(cx) {
                            Poll::Ready(Ok(())) => continue,
                            poll => poll,
                        }
                    }
                    Err(err) => Poll::Ready(Err(err)),
                };
            }
        }
    }
}

impl AsyncChannel {
    pub fn handle(&self) -> i32 {
        self.inner.as_raw_fd()
    }
}
