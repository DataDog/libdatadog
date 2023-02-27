// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{
    collections::{BTreeMap, VecDeque},
    io,
    os::unix::prelude::{AsRawFd, FromRawFd, IntoRawFd, RawFd},
};

use io_lifetimes::OwnedFd;
use spawn_worker::getpid;

use crate::ipc::{
    handles::{HandlesTransport, TransferHandles},
    platform::{Message, PlatformHandle, MAX_FDS},
};

#[derive(Debug)]
pub struct ChannelMetadata {
    fds_to_send: Vec<PlatformHandle<OwnedFd>>,
    fds_received: VecDeque<RawFd>,
    fds_acked: Vec<RawFd>,
    fds_to_close: BTreeMap<RawFd, PlatformHandle<OwnedFd>>,
    pid: libc::pid_t, // must always be set to current Process ID
}

impl Default for ChannelMetadata {
    fn default() -> Self {
        Self {
            fds_to_send: Default::default(),
            fds_received: Default::default(),
            fds_acked: Default::default(),
            fds_to_close: Default::default(),
            pid: getpid(),
        }
    }
}

impl HandlesTransport for &mut ChannelMetadata {
    type Error = io::Error;

    fn move_handle<'h, T>(self, handle: PlatformHandle<T>) -> Result<(), Self::Error> {
        self.enqueue_for_sending(handle);

        Ok(())
    }

    fn provide_handle<T>(self, hint: &PlatformHandle<T>) -> Result<PlatformHandle<T>, Self::Error> {
        self.find_handle(hint).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "can't provide expected handle for hint: {}",
                    hint.as_raw_fd()
                ),
            )
        })
    }
}

impl ChannelMetadata {
    pub fn unwrap_message<T>(&mut self, message: Message<T>) -> Result<T, io::Error>
    where
        T: TransferHandles,
    {
        {
            let fds_to_close = message
                .acked_handles
                .into_iter()
                .flat_map(|fd| self.fds_to_close.remove(&fd));

            // if ACK came from the same PID, it means there is a duplicate PlatformHandle instance in the same
            // process. Thus we should leak the handles allowing other PlatformHandle's to safely close
            if message.pid == self.pid {
                for h in fds_to_close {
                    h.into_owned_handle()
                        .map(|h| h.into_raw_fd())
                        .unwrap_or_default();
                }
            } else {
                // drain iterator closing all open file desriptors that were ACKed by the other party
                fds_to_close.last();
            }
        }
        let mut item = message.item;

        item.receive_handles(self)?;
        Ok(item)
    }

    pub fn create_message<T>(&mut self, item: T) -> Result<Message<T>, io::Error>
    where
        T: TransferHandles,
    {
        item.move_handles(&mut *self)?;

        let message = Message {
            item,
            acked_handles: self.fds_acked.drain(..).collect(),
            pid: self.pid,
        };

        Ok(message)
    }

    pub(crate) fn defer_close_handles<T>(&mut self, handles: Vec<PlatformHandle<T>>) {
        let handles = handles.into_iter().map(|h| (h.as_raw_fd(), h.to_untyped()));
        self.fds_to_close.extend(handles);
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
