use std::{
    os::unix::net::{UnixListener as StdUnixListener, UnixStream as StdUnixStream},
    path::Path,
    pin::Pin,
};

use futures::Future;
use tokio::net::{UnixListener, UnixStream};

use super::{ConnectionListener, ConnectorProvider, IntoConnectionListener, IntoConnectorProvider};

pub struct NamedSocket<P: AsRef<Path>> {
    path: P,
    listener: StdUnixListener,
}

impl<P: AsRef<Path>> NamedSocket<P> {
    pub fn new(path: P) -> anyhow::Result<Self> {
        let listener = StdUnixListener::bind(&path)?;
        Ok(NamedSocket { path, listener })
    }
}

impl<P: AsRef<Path>> IntoConnectionListener<UnixListener> for NamedSocket<P> {
    fn into_connection_listener(self) -> anyhow::Result<UnixListener> {
        Ok(UnixListener::from_std(self.listener)?)
    }
}

pub struct NamedSocketConnector<P: AsRef<Path>> {
    path: P,
}

impl<P: AsRef<Path>> IntoConnectorProvider<NamedSocketConnector<P>> for NamedSocket<P> {
    fn into_connector_provider(self) -> anyhow::Result<NamedSocketConnector<P>>
    where
        NamedSocketConnector<P>: super::ConnectorProvider,
    {
        Ok(NamedSocketConnector { path: self.path })
    }
}

impl<P: AsRef<Path>> ConnectorProvider for NamedSocketConnector<P> {
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
