// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::fmt::{Debug, Formatter, Pointer};
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::prelude::RawHandle;
use std::ptr::null_mut;
use winapi::shared::minwindef::ULONG;
use winapi::um::handleapi::{CloseHandle, DuplicateHandle};
use winapi::um::processthreadsapi::{GetCurrentProcess, OpenProcess};
use winapi::um::winnt::{DUPLICATE_SAME_ACCESS, HANDLE, PROCESS_DUP_HANDLE};

use crate::{
    handles::TransferHandles,
    platform::{Message, PlatformHandle},
};

// A small HANDLE wrapper, so that it can have impl Drop.
// We cannot impl Drop for ProcessHandle, otherwise it's closed during moving of ProcessHandle.
pub struct WrappedHANDLE(HANDLE);

impl Drop for WrappedHANDLE {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

// Deferred ProcessHandle getter
pub enum ProcessHandle {
    Handle(WrappedHANDLE),
    Pid(ULONG),
    Getter(Box<dyn Fn() -> io::Result<ProcessHandle>>),
}

unsafe impl Send for ProcessHandle {}

impl ProcessHandle {
    pub fn get(&mut self) -> io::Result<HANDLE> {
        match self {
            ProcessHandle::Handle(handle) => {
                return Ok(handle.0);
            }
            ProcessHandle::Pid(pid) => {
                let handle = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, *pid) };
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                *self = ProcessHandle::Handle(WrappedHANDLE(handle));
            }
            ProcessHandle::Getter(getter) => *self = getter()?,
        };
        self.get()
    }

    pub fn send_file_handle(&mut self, handle: RawHandle) -> io::Result<RawHandle> {
        let mut dup_handle: HANDLE = null_mut();
        unsafe {
            if DuplicateHandle(
                GetCurrentProcess(),
                handle as HANDLE,
                self.get()?,
                &mut dup_handle,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(dup_handle as RawHandle)
    }
}

impl Debug for ProcessHandle {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessHandle::Handle(handle) => Pointer::fmt(&handle.0, f),
            ProcessHandle::Pid(pid) => pid.fmt(f),
            ProcessHandle::Getter(_) => "<getter>".fmt(f),
        }
    }
}

#[derive(Debug)]
pub struct ChannelMetadata {
    handles_to_send: Vec<PlatformHandle<OwnedHandle>>,
    handles_received: HashMap<u64, u64>,
    process_handle: ProcessHandle,
}

impl ChannelMetadata {
    pub fn from_process_handle(process_handle: ProcessHandle) -> Self {
        Self {
            handles_to_send: Default::default(),
            handles_received: Default::default(),
            process_handle,
        }
    }

    pub fn unwrap_message<T>(&mut self, message: Message<T>) -> Result<T, io::Error>
    where
        T: TransferHandles,
    {
        let mut item = message.item;
        self.handles_received = message.handles;

        item.receive_handles(self)?;
        Ok(item)
    }

    pub fn create_message<T>(&mut self, item: T) -> Result<Message<T>, io::Error>
    where
        T: TransferHandles,
    {
        item.copy_handles(&mut *self)?;

        let mut handle_map = HashMap::new();
        for handle in self.handles_to_send.drain(..) {
            handle_map.insert(
                handle.fd as u64,
                self.process_handle
                    .send_file_handle(handle.as_raw_handle())? as u64,
            );
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

    pub fn process_handle(&mut self) -> Option<HANDLE> {
        self.process_handle.get().ok()
    }
}
