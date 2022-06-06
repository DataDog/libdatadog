use std::{
    os::unix::net::{UnixListener as StdUnixListener, UnixStream as StdUnixStream},
    path::{Path, PathBuf},
    pin::Pin,
};

use futures::Future;
use tokio::net::{UnixListener, UnixStream};

use super::*;

pub struct NamedSocket {
    path: PathBuf,
    listener: StdUnixListener,
}

impl NamedSocket {
    pub fn new<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let listener = StdUnixListener::bind(&path)?;
        let path = path.as_ref().to_path_buf();
        Ok(NamedSocket { path, listener })
    }
}

type UninitializedListener = Box<dyn FnOnce() -> UnixListener>;
impl ForkSafe for UninitializedListener {}

impl IpcSystem<UnixListener, NamedSocketConnector, UnixStreamWriterHandle> for NamedSocket {
    type UninitializedListener = UninitializedListener;

    fn into_pair(self) -> (Self::UninitializedListener, NamedSocketConnector) {
        let listener = self.listener;
        (
            Box::from(move || UnixListener::from_std(listener).unwrap()),
            NamedSocketConnector { path: self.path },
        )
    }
}

pub struct NamedSocketConnector {
    path: PathBuf,
}

impl ConnectorProvider for NamedSocketConnector {
    fn provide_connector(&self) -> anyhow::Result<StdUnixStream> {
        Ok(StdUnixStream::connect(&self.path)?)
    }
}

impl ConnectionListener for tokio::net::UnixListener {
    fn stream_accept<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<UnixStream>> + Send + 'a>> {
        let f = self.accept();

        Box::pin(async {
            match f.await {
                Ok(r) => Ok(r.0),
                Err(err) => Err(err.into()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::sockets::tests::abstract_basic_ipc_test;

    use super::NamedSocket;

    #[test]
    fn test_basic_socket_connection() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");
        let sock = NamedSocket::new(sock_path).unwrap();
        abstract_basic_ipc_test(sock);
        dir.close().unwrap()
    }
}
