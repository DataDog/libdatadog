use std::{
    os::unix::{
        net::{UnixDatagram as StdUnixDatagram, UnixStream as StdUnixStream},
        prelude::{AsRawFd, FromRawFd, RawFd},
    },
    pin::Pin,
};

use futures::{Future, FutureExt};
use sendfd::{RecvWithFd, SendWithFd};
use tokio::net::{UnixDatagram, UnixStream};

use crate::fork::ForkSafe;

use super::{ConnectionListener, IpcSystem, UnixStreamWriterHandle, WriterHandleProvider};

pub struct SocketReceiver {
    source: UnixDatagram,
}

impl SocketReceiver {
    async fn receive_fd(&self) -> anyhow::Result<RawFd> {
        let mut buf: [u8; 255] = [0; 255];
        let mut fds: [RawFd; 10] = [0; 10];

        loop {
            self.source.readable().await?;
            match self.source.recv_with_fd(&mut buf, &mut fds) {
                Ok((_, fds_size)) if fds_size > 0 => {
                    return Ok(fds[0]);
                }
                Ok((_, _)) => {
                    return Err(anyhow::format_err!("no file descriptors received"));
                }
                Err(ref e) if e.kind() == tokio::io::ErrorKind::WouldBlock => {
                    continue;
                }
                Err(e) => {
                    return Err(e.into());
                }
            }
        }
    }

    async fn receive_stream<'a>(&'a self) -> anyhow::Result<UnixStream> {
        let fd = self.receive_fd().await?;
        let stream = unsafe { StdUnixStream::from_raw_fd(fd) };
        Ok(UnixStream::from_std(stream)?)
    }
}

impl ConnectionListener for SocketReceiver {
    fn stream_accept<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<UnixStream>> + Send + 'a>> {
        self.receive_stream().boxed()
    }
}

pub struct SharedSocket {
    parent_socket: StdUnixDatagram,
    child_socket: StdUnixDatagram,
}

impl SharedSocket {
    pub fn init() -> anyhow::Result<Self> {
        let (parent_socket, child_socket) = StdUnixDatagram::pair()?;
        Ok(Self {
            parent_socket,
            child_socket,
        })
    }
}

type UninitializedListener = Box<dyn FnOnce() -> SocketReceiver>;
impl ForkSafe for UninitializedListener {}

impl IpcSystem<SocketReceiver, SharedSocketConnector, UnixStreamWriterHandle> for SharedSocket {
    type UninitializedListener = UninitializedListener;

    fn into_pair(self) -> (Self::UninitializedListener, SharedSocketConnector) {
        let child_socket = self.child_socket;
        (
            Box::from(move || SocketReceiver {
                source: UnixDatagram::from_std(child_socket).unwrap(),
            }),
            SharedSocketConnector {
                socket: self.parent_socket,
            },
        )
    }
}

pub struct SharedSocketConnector {
    socket: StdUnixDatagram,
}

impl WriterHandleProvider<UnixStreamWriterHandle> for SharedSocketConnector {
    fn take_writer_handle(&self) -> anyhow::Result<UnixStreamWriterHandle> {
        let (peer, own) = StdUnixStream::pair()?;
        let buf = [0; 100];
        let fds = [peer.as_raw_fd()];

        self.socket.send_with_fd(&buf, &fds)?;
        Ok(own.into())
    }
}

#[cfg(test)]
mod tests {
    use super::SharedSocket;
    use crate::sockets::tests::abstract_basic_ipc_test;

    #[test]
    fn test_basic_socket_connection() {
        let sock = SharedSocket::init().unwrap();
        abstract_basic_ipc_test(sock);
    }
}
