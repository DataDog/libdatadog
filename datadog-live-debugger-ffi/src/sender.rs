// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::send_data::serialize_debugger_payload;
use datadog_live_debugger::debugger_defs::DebuggerPayload;
use datadog_live_debugger::sender;
use datadog_live_debugger::sender::{
    debugger_intake_endpoint, generate_tags, Config, DebuggerType,
};
use libdd_common::tag::Tag;
use libdd_common::Endpoint;
use libdd_common_ffi::slice::AsBytes;
use libdd_common_ffi::{CharSlice, MaybeError};
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
            Err(e) => return MaybeError::Some(libdd_common_ffi::Error::from(format!("{:?}", e))),
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
    SymDB {
        data: OwnedCharSlice,
        content_type: String,
    },
}

/// Returns a static name for a debugger track, used for logging without
/// allocating a string on every send.
fn debugger_type_name(debugger_type: DebuggerType) -> &'static str {
    match debugger_type {
        DebuggerType::Diagnostics => "Diagnostics",
        DebuggerType::Snapshots => "Snapshots",
        DebuggerType::Logs => "Logs",
    }
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
            let (kind, len, result) = match data {
                SendData::Raw(ref vec, r#type) => (
                    debugger_type_name(r#type),
                    vec.len(),
                    sender::send(vec.as_slice(), &config, r#type, &tags).await,
                ),
                SendData::Wrapped(ref wrapped, r#type) => {
                    let bytes = wrapped.slice.as_bytes();
                    (
                        debugger_type_name(r#type),
                        bytes.len(),
                        sender::send(bytes, &config, r#type, &tags).await,
                    )
                }
                SendData::SymDB {
                    ref data,
                    ref content_type,
                } => {
                    let bytes = data.slice.as_bytes();
                    (
                        "SymDB",
                        bytes.len(),
                        sender::send_symdb(bytes, content_type, &config, &tags).await,
                    )
                }
            };

            if let Err(e) = result {
                warn!("Failed to send {kind} debugger data: {e:?}");
            } else {
                debug!("Successfully sent {len} byte {kind} debugger data payload");
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
    global_tags: libdd_common_ffi::Vec<Tag>,
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

fn spawn_sender_inner(config: Config, tags: String, handle: &mut *mut SenderHandle) -> MaybeError {
    let runtime = try_c!(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build());

    let (tx, mailbox) = mpsc::channel(5000);
    let config = Arc::new(config);

    *handle = Box::into_raw(Box::new(SenderHandle {
        join: std::thread::spawn(move || {
            runtime.block_on(sender_routine(config, tags, mailbox));
            runtime.shutdown_background();
        }),
        channel: tx,
    }));

    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_spawn_sender(
    endpoint: &Endpoint,
    tags: Box<String>,
    handle: &mut *mut SenderHandle,
) -> MaybeError {
    let mut config = Config::default();
    try_c!(config.set_endpoint(endpoint.clone()));
    spawn_sender_inner(config, *tags, handle)
}

/// Builds an [`Endpoint`] for sending debugger and SymDB payloads directly to
/// the Datadog intake (agentless), targeting `debugger-intake.{site}`. The
/// returned endpoint must be freed with `ddog_endpoint_drop`.
#[no_mangle]
pub extern "C" fn ddog_live_debugger_endpoint_from_site_and_api_key(
    site: CharSlice,
    api_key: CharSlice,
    endpoint: &mut *mut Endpoint,
) -> MaybeError {
    let site = site.to_utf8_lossy();
    let built = try_c!(debugger_intake_endpoint(
        site.as_ref(),
        api_key.to_utf8_lossy().to_string()
    ));
    *endpoint = Box::into_raw(Box::new(built));
    MaybeError::None
}

/// Creates an empty sender config. Configure it with the
/// `ddog_live_debugger_sender_config_*` functions, then hand it to
/// `ddog_live_debugger_spawn_sender_with_config` (which consumes it). If the
/// config is not spawned, free it with `ddog_live_debugger_sender_config_drop`.
#[no_mangle]
pub extern "C" fn ddog_live_debugger_sender_config_new() -> Box<Config> {
    Box::new(Config::default())
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_sender_config_set_endpoint(
    config: &mut Config,
    endpoint: &Endpoint,
) -> MaybeError {
    try_c!(config.set_endpoint(endpoint.clone()));
    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_sender_config_set_symdb_endpoint(
    config: &mut Config,
    endpoint: &Endpoint,
) -> MaybeError {
    try_c!(config.set_symdb_endpoint(endpoint.clone()));
    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_sender_config_add_additional_endpoint(
    config: &mut Config,
    endpoint: &Endpoint,
) -> MaybeError {
    try_c!(config.add_additional_debugger_endpoint(endpoint.clone()));
    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_sender_config_add_additional_symdb_endpoint(
    config: &mut Config,
    endpoint: &Endpoint,
) -> MaybeError {
    try_c!(config.add_additional_symdb_endpoint(endpoint.clone()));
    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_live_debugger_sender_config_drop(_: Box<Config>) {}

/// Spawns a sender from a fully-configured [`Config`], consuming it. Supports
/// SymDB and additional dual-ship endpoints, unlike
/// `ddog_live_debugger_spawn_sender`.
#[no_mangle]
pub extern "C" fn ddog_live_debugger_spawn_sender_with_config(
    config: Box<Config>,
    tags: Box<String>,
    handle: &mut *mut SenderHandle,
) -> MaybeError {
    spawn_sender_inner(*config, *tags, handle)
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

/// Enqueues a raw SymDB (symbol database) payload to be forwarded to the SymDB
/// intake. The body is sent verbatim with the given `content_type`; `data` is
/// owned and freed once sent. Returns `true` if the payload was enqueued.
#[no_mangle]
pub extern "C" fn ddog_live_debugger_send_symdb_data(
    handle: &mut SenderHandle,
    content_type: CharSlice,
    data: OwnedCharSlice,
) -> bool {
    handle
        .channel
        .try_send(SendData::SymDB {
            data,
            content_type: content_type.to_utf8_lossy().to_string(),
        })
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
