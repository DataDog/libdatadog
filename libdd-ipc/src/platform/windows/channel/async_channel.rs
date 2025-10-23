// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::platform::metadata::ProcessHandle;
use crate::platform::Channel;
use pin_project::pin_project;
use std::fmt::Debug;
use std::os::windows::io::AsRawHandle;
use std::{
    io,
    sync::{Arc, Mutex},
    task::Poll,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer};
use winapi::shared::wtypesbase::ULONG;
use winapi::um::winbase::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use winapi::um::winnt::HANDLE;

use super::ChannelMetadata;

#[derive(Debug)]
// Note: needs to be #[pin] because impls on AsyncChannel require #[pin]
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
    pub metadata: Arc<Mutex<ChannelMetadata>>,
}

macro_rules! use_inner {
    ($base:expr, $method:ident($($args:expr),*)) => {
        match $base.inner {
            NamedPipe::Client(ref client) => client.$method($($args),*),
            NamedPipe::Server(ref server) => server.$method($($args),*),
        }
    }
}

impl AsyncChannel {
    pub fn from_raw_and_process(pipe: NamedPipe, process_handle: ProcessHandle) -> AsyncChannel {
        AsyncChannel {
            inner: pipe,
            metadata: Arc::new(Mutex::new(ChannelMetadata::from_process_handle(
                process_handle,
            ))),
        }
    }

    pub fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        use_inner!(self, try_read(buf))
    }

    pub fn try_write(&self, buf: &[u8]) -> io::Result<usize> {
        use_inner!(self, try_write(buf))
    }

    pub fn handle(&self) -> i32 {
        use_inner!(self, as_raw_handle()) as i32
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

    fn try_from(value: Channel) -> Result<Self, Self::Error> {
        Ok(AsyncChannel {
            inner: unsafe {
                let handle = value.inner.handle.as_raw_handle();
                if value.inner.client {
                    NamedPipe::Client(NamedPipeClient::from_raw_handle(handle)?)
                } else {
                    NamedPipe::Server(NamedPipeServer::from_raw_handle(handle)?)
                }
            },
            metadata: Arc::new(Mutex::new(value.metadata)),
        })
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
