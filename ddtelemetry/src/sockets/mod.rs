pub mod named;
pub mod passfd;

use std::io;

use std::os::unix::net::UnixStream as StdUnixStream;

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures::Future;

use tokio::net::UnixStream;

use crate::fork::ForkSafe;

pub trait ConnectionListener {
    fn stream_accept<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<UnixStream>> + Send + 'a>>;
}

pub trait ConnectorProvider {
    fn provide_connector(&self) -> anyhow::Result<StdUnixStream>;
}

pub trait IntoConnectionListener<T> {
    fn into_connection_listener(self) -> anyhow::Result<T>
    where
        T: ConnectionListener;
}

pub trait IntoConnectorProvider<T> {
    fn into_connector_provider(self) -> anyhow::Result<T>
    where
        T: ConnectorProvider;
}

pub trait WriterHandleProvider<T>
where
    T: WriterHandle,
{
    fn take_writer_handle(&self) -> anyhow::Result<T>;
}

#[derive(Clone)]
pub struct UnixStreamWriterHandle(Arc<Mutex<StdUnixStream>>);

pub trait WriterHandle: Clone + Send + io::Write {}

fn mutex_error_as_io_error<T>(_: T) -> io::Error {
    io::Error::new(
        io::ErrorKind::Other,
        "failed aquiring mutex for shared socket",
    )
}

impl io::Write for UnixStreamWriterHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut writer = self.0.lock().map_err(mutex_error_as_io_error)?;
        writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut writer = self.0.lock().map_err(mutex_error_as_io_error)?;
        writer.flush()
    }
}

impl From<StdUnixStream> for UnixStreamWriterHandle {
    fn from(socket: StdUnixStream) -> Self {
        Self(Arc::from(Mutex::from(socket)))
    }
}

impl WriterHandle for UnixStreamWriterHandle {}

impl<P: ConnectorProvider> WriterHandleProvider<UnixStreamWriterHandle> for P {
    fn take_writer_handle(&self) -> anyhow::Result<UnixStreamWriterHandle> {
        let socket = self.provide_connector()?;
        Ok(UnixStreamWriterHandle::from(socket))
    }
}

// init
// sidecar_after_fork
// parent_after_sidecar
// child_after_fork

// handle

// parent process ->
// sidecar ->
// || child -> a
// || || initialize
// fork
//

pub trait IpcSystem<Listener, Provider, Handle>
where
    Listener: ConnectionListener,
    Provider: WriterHandleProvider<Handle>,
    Handle: WriterHandle,
{
    type UninitializedListener: Uninitialized<Listener> + ForkSafe + 'static;

    fn into_pair(self) -> (Self::UninitializedListener, Provider);
}

pub trait Uninitialized<T> {
    fn init(self) -> T;
}

impl<T, F> Uninitialized<T> for F
where
    F: FnOnce() -> T,
{
    fn init(self) -> T {
        self()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
    };

    use super::{ConnectionListener, IpcSystem, WriterHandle};
    use crate::{
        assert_child_exit,
        fork::{self},
        sockets::{Uninitialized, WriterHandleProvider},
    };
    use tokio::runtime;

    pub fn abstract_basic_ipc_test<T, Listener, Provider, Handle>(ipc: T)
    where
        T: IpcSystem<Listener, Provider, Handle> + 'static,
        Listener: ConnectionListener + 'static,
        Provider: WriterHandleProvider<Handle>,
        Handle: WriterHandle,
    {
        let (mut recv, send) = UnixStream::pair().unwrap();

        let (listener, provider) = ipc.into_pair();

        let pid = fork::safer_fork((listener, send), |(listener, mut send)| {
            fork::tests::set_fork_panic_handler();

            let runtime = runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            runtime
                .block_on(async {
                    let conn = listener.init().stream_accept().await.unwrap();
                    conn.readable().await.unwrap();
                    let mut buf = [0; 10];
                    let n = conn.try_read(&mut buf).unwrap();

                    conn.writable().await.unwrap();
                    send.write_fmt(format_args!(
                        "{}-mirror",
                        std::str::from_utf8(&buf[..n]).unwrap()
                    ))
                })
                .unwrap();
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut sock = provider.take_writer_handle().unwrap();
        sock.write("test".as_bytes()).unwrap();
        let mut buf = String::new();
        recv.read_to_string(&mut buf).unwrap();
        assert_eq!("test-mirror", buf);

        assert_child_exit!(pid);
    }
}
