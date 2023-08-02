// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use pin_project::pin_project;
use std::{
    io,
    sync::{Arc, Mutex},
    task::Poll,
};
use std::os::windows::io::RawHandle;
use tokio::{
    io::{AsyncRead, AsyncWrite},
};
use std::os::windows::io::AsRawHandle;
use std::ptr::null_mut;
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer};
use winapi::shared::wtypesbase::ULONG;
use winapi::um::handleapi::DuplicateHandle;
use winapi::um::processthreadsapi::{GetCurrentProcess, OpenProcess};
use winapi::um::winbase::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use winapi::um::winnt::{DUPLICATE_SAME_ACCESS, HANDLE, PROCESS_DUP_HANDLE};
use crate::platform::Channel;

use super::ChannelMetadata;

#[derive(Debug)]
#[pin_project(project = NamedPipeProject)]
enum NamedPipe {
    Server(#[pin] NamedPipeServer),
    Client(#[pin] NamedPipeClient),
}

#[derive(Debug)]
#[pin_project]
pub struct AsyncChannel {
    #[pin]
    inner: NamedPipe,
    process_handle: HANDLE,
    pub metadata: Arc<Mutex<ChannelMetadata>>,
}

macro_rules! use_inner {
    ($base:expr, $method:ident($($args:expr),+)) => {
        match $base.inner {
            NamedPipe::Client(ref client) => client.$method($($args),+),
            NamedPipe::Server(ref server) => server.$method($($args),+),
        }
    }
}

impl AsyncChannel {
    pub fn send_file_handle(&self, handle: RawHandle) -> io::Result<RawHandle> {
        let mut dup_handle: HANDLE = null_mut();
        unsafe {
            if !DuplicateHandle(GetCurrentProcess(), handle as HANDLE, self.process_handle, &mut dup_handle, 0, 0, DUPLICATE_SAME_ACCESS) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(dup_handle as RawHandle)
    }

    pub fn from_raw(server: bool, handle: RawHandle) -> Result<Self, io::Error> {
        let mut pid: ULONG = 0;
        let named_pipe= if server {
            let server = unsafe { NamedPipeServer::from_raw_handle(handle)? };
            unsafe {
                GetNamedPipeServerProcessId(handle as HANDLE, &mut pid);
            }
            NamedPipe::Server(server)
        } else {
            let client = unsafe { NamedPipeClient::from_raw_handle(handle)? };
            unsafe {
                GetNamedPipeClientProcessId(handle as HANDLE, &mut pid);
            }
            NamedPipe::Client(client)
        };
        Ok(AsyncChannel {
            inner: named_pipe,
            process_handle: unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, pid) },
            metadata: Arc::new(Mutex::new(ChannelMetadata::default())),
        })
    }

    pub fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        use_inner!(self, try_read(buf))
    }

    pub fn try_write(&self, buf: &[u8]) -> io::Result<usize> {
        use_inner!(self, try_write(buf))
    }
}

impl From<NamedPipeServer> for AsyncChannel {
    fn from(stream: NamedPipeServer) -> Self {
        let mut pid: ULONG = 0;
        unsafe {
            GetNamedPipeServerProcessId(stream.as_raw_handle() as HANDLE, &mut pid);
        }
        AsyncChannel {
            inner: NamedPipe::Server(stream),
            process_handle: unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, pid) },
            metadata: Arc::new(Mutex::new(ChannelMetadata::default())),
        }
    }
}

impl TryFrom<Channel> for AsyncChannel {
    type Error = io::Error;

    fn try_from(mut value: Channel) -> Result<Self, Self::Error> {
        Ok(value.inner.pipe.take().unwrap())
    }
}

macro_rules! pipe_inner {
    ($pin:expr, $method:ident($($args:expr),+)) => {
        match $pin.project().inner.project() {
            NamedPipeProject::Client(ref mut client) => client.as_mut().$method($($args),+),
            NamedPipeProject::Server(ref mut server) => server.as_mut().$method($($args),+),
        }
    }
}

impl AsyncWrite for AsyncChannel {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        pipe_inner!(self, poll_write(cx, buf))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        pipe_inner!(self, poll_flush(cx))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        pipe_inner!(self, poll_shutdown(cx))
    }
}

impl AsyncRead for AsyncChannel {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        pipe_inner!(self, poll_read(cx, buf))
    }
}
