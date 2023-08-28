// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::platform::Channel;
use pin_project::pin_project;
use std::fmt::{Debug, Formatter, Pointer};
use std::os::windows::io::AsRawHandle;
use std::os::windows::io::RawHandle;
use std::ptr::null_mut;
use std::{
    io,
    sync::{Arc, Mutex},
    task::Poll,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer};
use winapi::shared::wtypesbase::ULONG;
use winapi::um::handleapi::{CloseHandle, DuplicateHandle};
use winapi::um::processthreadsapi::{GetCurrentProcess, OpenProcess};
use winapi::um::winbase::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use winapi::um::winnt::{DUPLICATE_SAME_ACCESS, HANDLE, PROCESS_DUP_HANDLE};

use super::ChannelMetadata;

#[derive(Debug)]
#[pin_project(project = NamedPipeProject)]
pub enum NamedPipe {
    Server(#[pin] NamedPipeServer),
    Client(#[pin] NamedPipeClient),
}

#[derive(Debug)]
#[pin_project]
pub struct AsyncChannel {
    #[pin]
    inner: NamedPipe,
    process_handle: Arc<Mutex<ProcessHandle>>,
    pub metadata: Arc<Mutex<ChannelMetadata>>,
}

// A small HANDLE wrapper, so that it can have impl Drop.
// We cannot impl Drop for ProcessHandle, otherwise it's closed during moving of ProcessHandle.
pub struct WrappedHANDLE(HANDLE);

// Deferred ProcessHandle getter
pub enum ProcessHandle {
    Handle(WrappedHANDLE),
    Pid(ULONG),
    Getter(Box<dyn FnOnce() -> io::Result<ProcessHandle>>),
}

impl ProcessHandle {
    pub fn get(&mut self) -> io::Result<HANDLE> {
        match self {
            ProcessHandle::Handle(handle) => {
                return Ok(handle.0);
            }
            ProcessHandle::Pid(pid) => {
                let handle = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, *pid) };
                if handle == null_mut() {
                    return Err(io::Error::last_os_error());
                }
                *self = ProcessHandle::Handle(WrappedHANDLE(handle));
            }
            ProcessHandle::Getter(getter) => {
                *self = std::mem::replace(getter, Box::new(|| unreachable!()))()?
            }
        };
        return self.get();
    }
}

impl Debug for ProcessHandle {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessHandle::Handle(handle) => Pointer::fmt(&handle.0, f),
            ProcessHandle::Pid(pid) => pid.fmt(f),
            ProcessHandle::Getter(_) => "<getter>".fmt(f),
        }
    }
}

unsafe impl Send for AsyncChannel {}

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
            if DuplicateHandle(
                GetCurrentProcess(),
                handle as HANDLE,
                self.process_handle.lock().unwrap().get()?,
                &mut dup_handle,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(dup_handle as RawHandle)
    }

    pub fn from_raw(server: bool, handle: RawHandle) -> Result<Self, io::Error> {
        if server {
            Ok(AsyncChannel::from(unsafe {
                NamedPipeServer::from_raw_handle(handle)?
            }))
        } else {
            Ok(AsyncChannel::from(unsafe {
                NamedPipeClient::from_raw_handle(handle)?
            }))
        }
    }

    pub fn from_raw_and_process(pipe: NamedPipe, process_handle: ProcessHandle) -> AsyncChannel {
        AsyncChannel {
            inner: pipe,
            process_handle: Arc::new(Mutex::new(process_handle)),
            metadata: Arc::new(Mutex::new(ChannelMetadata::default())),
        }
    }

    pub fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        use_inner!(self, try_read(buf))
    }

    pub fn try_write(&self, buf: &[u8]) -> io::Result<usize> {
        use_inner!(self, try_write(buf))
    }
}

impl From<NamedPipeServer> for AsyncChannel {
    fn from(pipe: NamedPipeServer) -> Self {
        let mut pid: ULONG = 0;
        unsafe {
            GetNamedPipeClientProcessId(pipe.as_raw_handle() as HANDLE, &mut pid);
        }
        AsyncChannel::from_raw_and_process(NamedPipe::Server(pipe), ProcessHandle::Pid(pid))
    }
}

impl From<NamedPipeClient> for AsyncChannel {
    fn from(pipe: NamedPipeClient) -> Self {
        let mut pid: ULONG = 0;
        unsafe {
            GetNamedPipeServerProcessId(pipe.as_raw_handle() as HANDLE, &mut pid);
        }
        AsyncChannel::from_raw_and_process(NamedPipe::Client(pipe), ProcessHandle::Pid(pid))
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

impl Drop for WrappedHANDLE {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
