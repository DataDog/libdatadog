pub mod named;
pub mod passfd;

use std::os::unix::net::UnixStream as StdUnixStream;

use std::pin::Pin;

use futures::Future;

use tokio::net::UnixStream;

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

#[cfg(test)]
pub(crate) mod tests {
    use std::io::{Read, Write};

    use crate::fork;
    use tokio::runtime;

    use super::{
        ConnectionListener, ConnectorProvider, IntoConnectionListener, IntoConnectorProvider,
    };

    pub fn abstract_basic_ipc_test<T, Listener, Provider>(ipc: T)
    where
        T: IntoConnectionListener<Listener> + IntoConnectorProvider<Provider>,
        Listener: ConnectionListener,
        Provider: ConnectorProvider,
    {
        if let fork::Fork::Child = fork::fork().unwrap() {
            let runtime = runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            runtime.block_on(async {
                let listener = ipc.into_connection_listener().unwrap();

                let conn = listener.stream_accept().await.unwrap();
                conn.readable().await.unwrap();
                let mut buf = [0; 10];
                let n = conn.try_read(&mut buf).unwrap();

                conn.writable().await.unwrap();
                conn.try_write(&buf[..n]).unwrap();
                conn.try_write("mirror".as_bytes()).unwrap();
            });
            std::process::exit(0);
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut sock = ipc
            .into_connector_provider()
            .unwrap()
            .provide_connector()
            .unwrap();
        sock.write("test".as_bytes()).unwrap();

        let mut buf = [0; 10];
        let n = sock.read(&mut buf).unwrap();

        assert_eq!("testmirror".as_bytes(), &buf[..n]);
    }
}
