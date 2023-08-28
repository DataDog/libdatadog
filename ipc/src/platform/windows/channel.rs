// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::handles::TransferHandles;
use crate::platform::{AsyncChannel, Message};
use std::future::Future;
use std::os::windows::io::IntoRawHandle;
use std::{
    io::{self, Read, Write},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeClient;
use tokio::runtime::Handle;
use tokio::time::timeout;

pub mod async_channel;
pub mod metadata;

use self::metadata::ChannelMetadata;

use super::super::PlatformHandle;

#[derive(Debug)]
struct Inner {
    pipe: Option<AsyncChannel>,
    blocking: bool,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    runtime: Handle,
}

#[derive(Debug)]
pub struct Channel {
    inner: Inner,
    pub metadata: ChannelMetadata,
}

/*
impl Clone for Channel {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            metadata: Default::default(),
        }
    }
}
*/

impl Channel {
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.read_timeout = timeout;
        Ok(())
    }

    pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.write_timeout = timeout;
        Ok(())
    }

    pub fn set_nonblocking(&mut self, nonblocking: bool) -> io::Result<()> {
        self.inner.blocking = !nonblocking;
        Ok(())
    }

    pub fn probe_readable(&self) -> bool {
        let mut buf = [0u8; 1];
        self.inner.pipe.as_ref().unwrap().try_read(&mut buf).is_ok()
    }

    async fn do_wait<'a, O, F, Fut>(
        pipe: &'a mut AsyncChannel,
        call: F,
        duration: Option<Duration>,
    ) -> Result<O, io::Error>
    where
        F: FnOnce(&'a mut AsyncChannel) -> Fut,
        Fut: Future<Output = Result<O, io::Error>> + 'a,
    {
        let future = call(pipe);
        if let Some(duration) = duration {
            match timeout(duration, future).await {
                Ok(o) => o,
                Err(e) => Err(io::Error::from(e)),
            }
        } else {
            future.await
        }
    }

    fn wait_io_future<'a, O, F, Fut>(
        &'a mut self,
        call: F,
        duration: Option<Duration>,
    ) -> Result<O, io::Error>
    where
        F: FnOnce(&'a mut AsyncChannel) -> Fut,
        Fut: Future<Output = Result<O, io::Error>> + 'a,
    {
        let pipe = self.inner.pipe.as_mut().unwrap();
        self.inner
            .runtime
            .block_on(Self::do_wait(pipe, call, duration))
    }

    pub fn create_message<T>(&mut self, item: T) -> Result<Message<T>, io::Error>
    where
        T: TransferHandles,
    {
        self.metadata
            .create_message(item, self.inner.pipe.as_ref().unwrap())
    }
}

impl Read for Channel {
    fn read<'a>(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.inner.blocking {
            self.wait_io_future(|p| p.read(buf), self.inner.read_timeout)
        } else {
            self.inner.pipe.as_ref().unwrap().try_read(buf)
        }
    }
}

impl Write for Channel {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.inner.blocking {
            self.wait_io_future(|p| p.write(buf), self.inner.write_timeout)
        } else {
            self.inner.pipe.as_ref().unwrap().try_write(buf)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.wait_io_future(|p| p.flush(), self.inner.write_timeout)
    }
}

/*
impl From<Channel> for PlatformHandle<UnixStream> {
    fn from(c: Channel) -> Self {
        c.inner
    }
}
*/

impl From<PlatformHandle<NamedPipeClient>> for Channel {
    fn from(h: PlatformHandle<NamedPipeClient>) -> Self {
        Channel::from(
            AsyncChannel::from_raw(false, h.into_owned_handle().unwrap().into_raw_handle())
                .unwrap(),
        )
    }
}

impl From<NamedPipeClient> for Channel {
    fn from(stream: NamedPipeClient) -> Self {
        Channel::from(AsyncChannel::from(stream))
    }
}

impl From<AsyncChannel> for Channel {
    fn from(channel: AsyncChannel) -> Self {
        Channel {
            inner: Inner {
                pipe: Some(channel),
                blocking: true,
                read_timeout: None,
                write_timeout: None,
                runtime: Handle::current(),
            },
            metadata: Default::default(),
        }
    }
}
