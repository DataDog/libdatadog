// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::send_data::serialize_debugger_payload;
use datadog_live_debugger::debugger_defs::DebuggerPayload;
use datadog_live_debugger::sender;
use datadog_live_debugger::sender::{generate_tags, Config, DebuggerType};
use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use ddcommon_ffi::slice::AsBytes;
use ddcommon_ffi::{CharSlice, MaybeError};
use log::{debug, warn};
use percent_encoding::{percent_encode, CONTROLS};
use std::sync::Arc;
use std::thread::JoinHandle;
use tokio::sync::mpsc;
use tokio_util::task::TaskTracker;

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
    Raw(Vec<u8>, DebuggerType),
    Wrapped(OwnedCharSlice, DebuggerType),
}

async fn sender_routine(config: Arc<Config>, tags: String, mut receiver: mpsc::Receiver<SendData>) {
    let tags = Arc::new(tags);
    let tracker = TaskTracker::new();
    loop {
        let data = match receiver.recv().await {
            None => break,
            Some(data) => data,
        };

        let config = config.clone();
        let tags = tags.clone();
        tracker.spawn(async move {
            let (debugger_type, data) = match data {
                SendData::Raw(ref vec, r#type) => (r#type, vec.as_slice()),
                SendData::Wrapped(ref wrapped, r#type) => (r#type, wrapped.slice.as_bytes()),
            };

            if let Err(e) = sender::send(data, &config, debugger_type, &tags).await {
                warn!("Failed to send {debugger_type:?} debugger data: {e:?}");
            } else {
                debug!(
                    "Successfully sent {} debugger data byte {debugger_type:?} payload",
                    data.len()
                );
            }
        });
    }

    tracker.wait().await;
}

pub struct SenderHandle {
    join: JoinHandle<()>,
    channel: mpsc::Sender<SendData>,
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_build_tags(
    debugger_version: CharSlice,
    env: CharSlice,
    version: CharSlice,
    runtime_id: CharSlice,
    global_tags: ddcommon_ffi::Vec<Tag>,
) -> Box<String> {
    Box::new(generate_tags(
        &debugger_version.to_utf8_lossy(),
        &env.to_utf8_lossy(),
        &version.to_utf8_lossy(),
        &runtime_id.to_utf8_lossy(),
        &mut global_tags.into_iter(),
    ))
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_tags_from_raw(tags: CharSlice) -> Box<String> {
    Box::new(percent_encode(tags.as_bytes(), CONTROLS).to_string())
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_spawn_sender(
    endpoint: &Endpoint,
    tags: Box<String>,
    handle: &mut *mut SenderHandle,
) -> MaybeError {
    let runtime = try_c!(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build());

    let (tx, mailbox) = mpsc::channel(5000);
    let mut config = Config::default();
    try_c!(config.set_endpoint(endpoint.clone(), endpoint.clone()));
    let config = Arc::new(config);

    *handle = Box::into_raw(Box::new(SenderHandle {
        join: std::thread::spawn(move || {
            runtime.block_on(sender_routine(config, *tags, mailbox));
            runtime.shutdown_background();
        }),
        channel: tx,
    }));

    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_send_raw_data(
    handle: &mut SenderHandle,
    debugger_type: DebuggerType,
    data: OwnedCharSlice,
) -> bool {
    handle
        .channel
        .try_send(SendData::Wrapped(data, debugger_type))
        .is_ok()
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_send_payload(
    handle: &mut SenderHandle,
    data: &DebuggerPayload,
) -> bool {
    let debugger_type = DebuggerType::of_payload(data);
    handle
        .channel
        .try_send(SendData::Raw(
            serialize_debugger_payload(data).into_bytes(),
            debugger_type,
        ))
        .is_err()
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_live_debugger_drop_sender(sender: *mut SenderHandle) {
    drop(Box::from_raw(sender));
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_live_debugger_join_sender(sender: *mut SenderHandle) {
    let sender = Box::from_raw(sender);
    drop(sender.channel);
    _ = sender.join.join();
}
