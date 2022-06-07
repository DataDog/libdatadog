use std::{
    fmt::Debug,
    io::Write,
    sync::{atomic::{AtomicU32, Ordering}, Arc},
};

use bytes::BytesMut;
use ddtelemetry::{
    fork::{self, ForkSafe, Forkable},
    sockets::{
        self, ConnectionListener, ConnectorProvider, IntoConnectionListener, IntoConnectorProvider,
        IpcSystem, Uninitialized, WriterHandle, WriterHandleProvider,
    },
};
use futures::TryStreamExt;
use nix::sys::wait::{waitpid, WaitPidFlag};
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;
use tokio_serde::{formats::SymmetricalBincode, Serializer};
use tokio_util::codec::{Encoder, FramedRead, LengthDelimitedCodec};

static SEQ: AtomicU32 = AtomicU32::new(0);

#[derive(Serialize, Deserialize, Clone)]
pub struct Payload {
    seq: u32,
    pid: libc::pid_t,
    data: Box<[u8]>,
}

impl Default for Payload {
    fn default() -> Self {
        Self {
            seq: SEQ.fetch_add(1, Ordering::SeqCst),
            pid: fork::getpid(),
            data: vec![0; 2].into(),
        }
    }
}

impl Debug for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Payload")
            .field("seq", &self.seq)
            .field("pid", &self.pid)
            .field("data", &format!("<{}>", self.data.len()))
            .finish()
    }
}

impl Payload {
    fn to_lenght_delimeted_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let mut codec = LengthDelimitedCodec::new();
        let mut enc_sym = SymmetricalBincode::<Payload>::default();
        let enc = std::pin::Pin::new(&mut enc_sym);
        let tmp = enc.serialize(self)?;
        // TODO: plain bincode serialize produces different results (possibly because of a version missmatch). for now lets ab-use SymmetricalBincode
        // let tmp = bincode::serialize(self)?.into();
        let mut out = BytesMut::new();
        codec.encode(tmp, &mut out)?;

        Ok(out.to_vec())
    }
}
fn child<T>(mut socket: T) -> anyhow::Result<()>
where
    T: WriterHandle,
{
    socket.write_all(&Payload::default().to_lenght_delimeted_bytes()?)?;
    socket.write_all(&Payload::default().to_lenght_delimeted_bytes()?)?;
    socket.write_all(&Payload::default().to_lenght_delimeted_bytes()?)?;
    socket.flush()?;
    std::process::exit(0);
}

fn spawn_multiple<T, W>(provider: T) -> anyhow::Result<()>
where
    T: WriterHandleProvider<W> + 'static,
    W: WriterHandle + 'static,
{
    let provider = Arc::new(provider);
    for i in 0..2 {
        fork::safer_fork(
            (i, Forkable::mark_as(provider.clone())),
            |(i, handle)| {
                println!("child {}", i);
                set_fork_panic_handler();

                // child(handle.take().take_writer_handle().unwrap() ).unwrap();
            },
        )
        .unwrap();
    }
    Ok(())
}

async fn handle_messages(stream: UnixStream) -> anyhow::Result<()> {
    stream.readable().await?;

    let length_delimited = FramedRead::new(stream, LengthDelimitedCodec::new());
    let mut deserialized = tokio_serde::SymmetricallyFramed::<_, Payload, _>::new(
        length_delimited,
        SymmetricalBincode::<Payload>::default(),
    );

    while let Some(msg) = deserialized.try_next().await? {
        println!("rcvd: {:?}", msg);
    }
    println!("handler-exiting");
    Ok(())
}

async fn agent_loop<T>(listener: T) -> anyhow::Result<()>
where
    T: ConnectionListener,
{
    loop {
        let stream = listener.stream_accept().await?;
        tokio::spawn(async move { handle_messages(stream).await });
    }
}

fn agent<T, U>(listener: U) -> anyhow::Result<()>
where
    T: ConnectionListener + 'static,
    U: Uninitialized<T>,
{
    // let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let listener = listener.init();
        agent_loop(listener).await
    })
}

pub fn set_fork_panic_handler() {
    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |p| {
        old_hook(p);
        std::process::exit(1);
    }));
}

fn main() -> anyhow::Result<()> {
    // let ipc =
    // sockets::named::NamedSocket::new(format!("/tmp/ddtelemetry-{}.sock", fork::getpid()))?;
    let ipc = sockets::passfd::SharedSocket::init()?; // some race conditions still seem to exist

    let (listener, connector) = ipc.into_pair();

    let pid = fork::safer_fork(listener, |listener| {
        set_fork_panic_handler();
        agent(listener).unwrap();
    })
    .unwrap();
    spawn_multiple(connector)?;

    let res = waitpid(Some(nix::unistd::Pid::from_raw(pid)), Some(WaitPidFlag::WEXITED))?;
    println!("a {:?}", res);
    Ok(())
}
