// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::handles::TransferHandles;
use crate::platform::Message;
use std::os::windows::io::AsRawHandle;
use std::{
    io::{self, Read, Write},
    time::Duration,
};
use std::ffi::c_void;
use std::fmt::{Debug, Formatter, Pointer};
use std::os::windows::prelude::OwnedHandle;
use std::ptr::{null, null_mut};
use winapi::um::winbase::INFINITE;
use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED, OVERLAPPED_0};
use windows_sys::Win32::System::Pipes::{PeekNamedPipe, PIPE_NOWAIT, PIPE_WAIT, SetNamedPipeHandleState};
use windows_sys::Win32::System::Threading::{CreateEventA, WaitForSingleObject};
use crate::platform::metadata::ProcessHandle;

pub mod async_channel;
pub mod metadata;

use self::metadata::ChannelMetadata;

struct Inner {
    overlapped: OVERLAPPED,
    handle: OwnedHandle,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    blocking: bool,
    client: bool,
}

impl Debug for Inner {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Pointer::fmt(&self.handle.as_raw_handle(), f)
    }
}

#[derive(Debug)]
pub struct Channel {
    inner: Inner,
    pub metadata: ChannelMetadata,
}

impl Channel {
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.read_timeout = timeout;
        Ok(())
    }

    pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.write_timeout = timeout;
        Ok(())
    }

    pub fn set_nonblocking(&mut self, nonblocking: bool) -> io::Result<()> {
        self.inner.blocking = !nonblocking;
        let mode = if nonblocking { PIPE_NOWAIT } else { PIPE_WAIT };
        if unsafe { SetNamedPipeHandleState(self.inner.handle.as_raw_handle() as HANDLE, &mode, null(), null()) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn probe_readable(&self) -> bool {
        let mut available_bytes = 0;
        if unsafe { PeekNamedPipe(self.inner.handle.as_raw_handle() as HANDLE, null_mut(), 0, null_mut(), &mut available_bytes, null_mut()) } != 0 {
            available_bytes > 0
        } else {
            true
        }
    }

    fn wait_io_overlapped(
        &mut self,
        duration: Option<Duration>,
    ) -> Result<usize, io::Error>
    {
        match unsafe { WaitForSingleObject(self.inner.overlapped.hEvent, duration.map(|d| d.as_millis() as u32).unwrap_or(INFINITE)) } {
            WAIT_OBJECT_0 => {
                let mut transferred: u32 = 0;
                if unsafe { GetOverlappedResult(self.inner.handle.as_raw_handle() as HANDLE, &self.inner.overlapped, &mut transferred, 1) } == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(transferred as usize)
                }
            },
            e @ _ => Err(io::Error::from_raw_os_error(e as i32)),
        }
    }

    pub fn create_message<T>(&mut self, item: T) -> Result<Message<T>, io::Error>
    where
        T: TransferHandles,
    {
        self.metadata.create_message(item)
    }

    pub fn from_client_handle_and_pid(h: OwnedHandle, pid: ProcessHandle) -> Channel {
        Channel {
            inner: Inner {
                overlapped: OVERLAPPED {
                    Internal: 0,
                    InternalHigh: 0,
                    Anonymous: OVERLAPPED_0 {
                        Pointer: null_mut(),
                    },
                    hEvent: unsafe { CreateEventA(null_mut(), 1, 0, null_mut()) },
                },
                handle: h,
                read_timeout: None,
                write_timeout: None,
                blocking: true,
                client: true,
            },
            metadata: ChannelMetadata::from_process_handle(pid),
        }
    }
}

impl Read for Channel {
    fn read<'a>(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if unsafe { ReadFile(self.inner.handle.as_raw_handle() as HANDLE, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, null_mut(), &mut self.inner.overlapped as *mut OVERLAPPED) } != 0 {
            self.wait_io_overlapped(self.inner.read_timeout)
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Write for Channel {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if unsafe { WriteFile(self.inner.handle.as_raw_handle() as HANDLE, buf.as_ptr(), buf.len() as u32, null_mut(), &mut self.inner.overlapped as *mut OVERLAPPED) } != 0 {
            self.wait_io_overlapped(self.inner.write_timeout)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // No-op on windows named pipes
        Ok(())
    }
}
