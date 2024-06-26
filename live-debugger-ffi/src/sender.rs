use crate::send_data::serialize_debugger_payload;
use datadog_live_debugger::debugger_defs::DebuggerPayload;
use datadog_live_debugger::sender;
use ddcommon::Endpoint;
use ddcommon_ffi::slice::AsBytes;
use ddcommon_ffi::{CharSlice, MaybeError};
use log::warn;
use std::sync::Arc;
use std::thread::JoinHandle;
use tokio::sync::mpsc;

macro_rules! try_c {
    ($failable:expr) => {
        match $failable {
            Ok(o) => o,
            Err(e) => return MaybeError::Some(ddcommon_ffi::Error::from(format!("{:?}", e))),
        }
    };
}

#[repr(C)]
pub struct OwnedCharSlice {
    slice: CharSlice<'static>,
    free: extern "C" fn(CharSlice<'static>),
}

unsafe impl Send for OwnedCharSlice {}

impl Drop for OwnedCharSlice {
    fn drop(&mut self) {
        (self.free)(self.slice)
    }
}

enum SendData {
    Raw(Vec<u8>),
    Wrapped(OwnedCharSlice),
}

async fn sender_routine(endpoint: Arc<Endpoint>, mut receiver: mpsc::Receiver<SendData>) {
    loop {
        let data = match receiver.recv().await {
            None => break,
            Some(data) => data,
        };

        let endpoint = endpoint.clone();
        tokio::spawn(async move {
            let data = match &data {
                SendData::Raw(vec) => vec.as_slice(),
                SendData::Wrapped(wrapped) => wrapped.slice.as_bytes(),
            };

            if let Err(e) = sender::send(data, &endpoint).await {
                warn!("Failed to send debugger data: {e:?}");
            }
        });
    }
}

pub struct SenderHandle {
    join: JoinHandle<()>,
    channel: mpsc::Sender<SendData>,
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_spawn_sender(
    endpoint: &Endpoint,
    handle: &mut *mut SenderHandle,
) -> MaybeError {
    let runtime = try_c!(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build());

    let (tx, mailbox) = mpsc::channel(5000);
    let endpoint = Arc::new(endpoint.clone());

    *handle = Box::into_raw(Box::new(SenderHandle {
        join: std::thread::spawn(move || {
            runtime.block_on(sender_routine(endpoint, mailbox));
            runtime.shutdown_background();
        }),
        channel: tx,
    }));

    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_send_raw_data(
    handle: &mut SenderHandle,
    data: OwnedCharSlice,
) -> bool {
    handle.channel.try_send(SendData::Wrapped(data)).is_err()
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_send_payload(
    handle: &mut SenderHandle,
    data: &DebuggerPayload,
) -> bool {
    handle
        .channel
        .try_send(SendData::Raw(serialize_debugger_payload(data).into_bytes()))
        .is_err()
}

#[no_mangle]
pub unsafe extern "C" fn ddog_live_debugger_drop_sender(sender: *mut SenderHandle) {
    drop(Box::from_raw(sender));
}

#[no_mangle]
pub unsafe extern "C" fn ddog_live_debugger_join_sender(sender: *mut SenderHandle) {
    let sender = Box::from_raw(sender);
    drop(sender.channel);
    _ = sender.join.join();
}
