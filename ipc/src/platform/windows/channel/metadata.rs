// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::io;
use std::collections::HashMap;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::prelude::RawHandle;

use crate::{
    handles::TransferHandles,
    platform::{Message, PlatformHandle},
};
use crate::platform::AsyncChannel;

#[derive(Debug)]
pub struct ChannelMetadata {
    handles_to_send: Vec<PlatformHandle<OwnedHandle>>,
    handles_received: HashMap<u64, u64>,
}

impl Default for ChannelMetadata {
    fn default() -> Self {
        Self {
            handles_to_send: Default::default(),
            handles_received: Default::default(),
        }
    }
}

impl ChannelMetadata {
    pub fn unwrap_message<T>(&mut self, message: Message<T>) -> Result<T, io::Error>
    where
        T: TransferHandles,
    {
        let mut item = message.item;
        self.handles_received = message.handles;

        item.receive_handles(self)?;
        Ok(item)
    }

    pub fn create_message<T>(&mut self, item: T, channel: &AsyncChannel) -> Result<Message<T>, io::Error>
    where
        T: TransferHandles,
    {
        item.move_handles(&mut *self)?;

        let mut handle_map = HashMap::new();
        for handle in self.handles_to_send.drain(..) {
            handle_map.insert(handle.fd as u64, channel.send_file_handle(handle.as_raw_handle())? as u64);
        }

        let message = Message {
            item,
            handles: handle_map,
        };

        Ok(message)
    }

    pub(crate) fn enqueue_for_sending<T>(&mut self, handle: PlatformHandle<T>) {
        self.handles_to_send.push(handle.to_untyped())
    }

    pub(crate) fn find_handle<T>(&mut self, hint: &PlatformHandle<T>) -> Option<PlatformHandle<T>> {
        if hint.as_raw_handle() < 0 as RawHandle {
            return Some(hint.clone());
        }

        let fd = self.handles_received.get(&(hint.as_raw_handle() as u64));

        fd.map(|handle| unsafe { PlatformHandle::from_raw_handle(*handle as RawHandle) })
    }
}
