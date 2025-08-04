// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::VecDeque,
    io,
    os::unix::prelude::{AsRawFd, FromRawFd, RawFd},
};

use io_lifetimes::OwnedFd;

use crate::{
    handles::TransferHandles,
    platform::{Message, PlatformHandle, MAX_FDS},
};

#[derive(Debug)]
pub struct ChannelMetadata {
    fds_to_send: Vec<PlatformHandle<OwnedFd>>,
    fds_received: VecDeque<RawFd>,
    pid: libc::pid_t, // must always be set to current Process ID
}

impl Default for ChannelMetadata {
    fn default() -> Self {
        Self {
            fds_to_send: Default::default(),
            fds_received: Default::default(),
            pid: nix::unistd::getpid().as_raw(),
        }
    }
}

impl ChannelMetadata {
    pub fn unwrap_message<T>(&mut self, message: Message<T>) -> Result<T, io::Error>
    where
        T: TransferHandles,
    {
        let mut item = message.item;

        item.receive_handles(self)?;
        Ok(item)
    }

    pub fn create_message<T>(&mut self, item: T) -> Result<Message<T>, io::Error>
    where
        T: TransferHandles,
    {
        item.copy_handles(&mut *self)?;

        let message = Message {
            item,
            pid: self.pid,
        };

        Ok(message)
    }

    pub(crate) fn enqueue_for_sending<T>(&mut self, handle: PlatformHandle<T>) {
        self.fds_to_send.push(handle.to_untyped())
    }

    pub(crate) fn reenqueue_for_sending(&mut self, mut handles: Vec<PlatformHandle<OwnedFd>>) {
        handles.append(&mut self.fds_to_send);
        self.fds_to_send = handles;
    }

    pub(crate) fn drain_to_send(&mut self) -> Vec<PlatformHandle<OwnedFd>> {
        let drain = self.fds_to_send.drain(..);

        let mut cnt: i32 = MAX_FDS.try_into().unwrap_or(i32::MAX);

        let (to_send, leftover) = drain.partition(|_| {
            cnt -= 1;
            cnt >= 0
        });
        self.reenqueue_for_sending(leftover);

        to_send
    }

    pub(crate) fn receive_fds(&mut self, fds: &[RawFd]) {
        self.fds_received.append(&mut fds.to_vec().into());
    }

    pub(crate) fn find_handle<T>(&mut self, hint: &PlatformHandle<T>) -> Option<PlatformHandle<T>> {
        if hint.as_raw_fd() < 0 {
            return Some(hint.clone());
        }

        let fd = self.fds_received.pop_front();

        fd.map(|fd| unsafe { PlatformHandle::from_raw_fd(fd) })
    }
}
