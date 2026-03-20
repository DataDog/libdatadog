// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
use std::os::windows::process::ExitStatusExt;
use std::process::ExitStatus;
use std::{env, io, mem};
use kernel32::WaitForSingleObject;
use winapi::WAIT_OBJECT_0;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::GetExitCodeProcess;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS, INVALID_HANDLE_VALUE};
use windows::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcess, InitializeProcThreadAttributeList,
    UpdateProcThreadAttribute, CREATE_DEFAULT_ERROR_MODE, CREATE_NEW_PROCESS_GROUP,
    CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS, EXTENDED_STARTUPINFO_PRESENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, STARTUPINFOW_FLAGS,
};
use kernel32::CreateFileA;
use winapi::{
    DWORD, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, GENERIC_READ, GENERIC_WRITE, LPCSTR, OPEN_EXISTING, SECURITY_ATTRIBUTES,
};
use std::ptr::null_mut;

use crate::ENV_PASS_FD_KEY;

pub enum Stdio {
    Handle(OwnedHandle),
    Null,
}

impl From<&File> for Stdio {
    fn from(value: &File) -> Self {
        Stdio::Handle(unsafe {
            let handle = value.as_raw_handle();
            OwnedHandle::from_raw_handle(if handle.is_null() {
                handle
            } else {
                let mut ret: HANDLE = Default::default();
                let cur_proc = GetCurrentProcess();
                DuplicateHandle(
                    cur_proc,
                    HANDLE(handle as isize),
                    cur_proc,
                    &mut ret as *mut HANDLE,
                    0,
                    true,
                    DUPLICATE_SAME_ACCESS,
                )
                .unwrap();
                ret.0 as RawHandle
            })
        })
    }
}

pub struct Child {
    pub handle: OwnedHandle,
    pub main_thread_handle: OwnedHandle,
}

impl Child {
    pub fn wait(&self) -> io::Result<ExitStatus> {
        unsafe {
            let res = WaitForSingleObject(self.handle.as_raw_handle(), u32::MAX);
            let mut status = 0;
            if res != WAIT_OBJECT_0 {
                return Err(io::Error::last_os_error());
            }
            GetExitCodeProcess(HANDLE(self.handle.as_raw_handle() as isize), &mut status)?;
            Ok(ExitStatus::from_raw(status))
        }
    }
}

/// Spawn the sidecar binary as a detached process, passing `handle_to_pass`
/// via the `__DD_INTERNAL_PASSED_FD` environment variable.
///
/// The handle is made inheritable by the child via
/// `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`.  `DETACHED_PROCESS` takes the place
/// of the Unix double-fork: the child has no console and is fully detached.
pub fn spawn_exec_binary(
    binary_path: &std::path::Path,
    subcommand: &str,
    env: &[(OsString, OsString)],
    handle_to_pass: OwnedHandle,
    stdout: Stdio,
    stderr: Stdio,
) -> anyhow::Result<Child> {
    // Build environment: caller env + the handle value
    let mut envs: Vec<(OsString, OsString)> = env.to_vec();
    envs.push((
        OsString::from(ENV_PASS_FD_KEY),
        OsString::from((handle_to_pass.as_raw_handle() as u64).to_string()),
    ));

    // Command line: "<binary>" "<subcommand>"
    let mut cmd: Vec<u16> = Vec::new();
    cmd.push(b'"' as u16);
    cmd.extend(binary_path.as_os_str().encode_wide());
    cmd.push(b'"' as u16);
    cmd.push(' ' as u16);
    cmd.push(b'"' as u16);
    cmd.extend(OsStr::new(subcommand).encode_wide());
    cmd.push(b'"' as u16);
    cmd.push(0);

    let mut envp: Vec<u16> = Vec::new();
    for (key, val) in &envs {
        envp.extend(key.encode_wide());
        envp.push('=' as u16);
        envp.extend(val.encode_wide());
        envp.push(0);
    }
    envp.push(0);

    let mut program: Vec<u16> = Vec::new();
    program.extend(binary_path.as_os_str().encode_wide());
    program.push(0);

    let stdin_h = open_null(true);
    let stdout_h = raw_handle_from_stdio(stdout, false);
    let stderr_h = raw_handle_from_stdio(stderr, false);

    let mut inherited_handles = vec![
        HANDLE(handle_to_pass.as_raw_handle() as isize),
        stdin_h,
        stdout_h,
        stderr_h,
    ];

    // Allocate a process attribute list for handle inheritance
    let mut attr_size: usize = 0;
    let _ = unsafe {
        InitializeProcThreadAttributeList(
            LPPROC_THREAD_ATTRIBUTE_LIST(null_mut()),
            1,
            0,
            &mut attr_size,
        )
    };
    let mut attr_buf: Vec<u8> = vec![0u8; attr_size];
    let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut _);
    unsafe { InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size).unwrap() };
    unsafe {
        UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(inherited_handles.as_mut_ptr() as *mut _),
            inherited_handles.len() * mem::size_of::<HANDLE>(),
            None,
            None,
        )
        .unwrap()
    };

    let mut pi = zeroed_process_information();
    let mut si = STARTUPINFOEXW {
        StartupInfo: zeroed_startupinfo(),
        lpAttributeList: attr_list,
    };
    si.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
    si.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = stdin_h;
    si.StartupInfo.hStdOutput = stdout_h;
    si.StartupInfo.hStdError = stderr_h;

    unsafe {
        CreateProcessW(
            PCWSTR(program.as_ptr()),
            PWSTR(cmd.as_mut_ptr()),
            None,
            None,
            true,
            CREATE_UNICODE_ENVIRONMENT
                | DETACHED_PROCESS
                | CREATE_NEW_PROCESS_GROUP
                | CREATE_DEFAULT_ERROR_MODE
                | EXTENDED_STARTUPINFO_PRESENT,
            Some(envp.as_mut_ptr() as *mut _),
            PCWSTR::null(),
            &si.StartupInfo,
            &mut pi,
        )
        .map_err(|e| anyhow::anyhow!("CreateProcessW failed: {e}"))?;

        Ok(Child {
            handle: OwnedHandle::from_raw_handle(pi.hProcess.0 as *mut _),
            main_thread_handle: OwnedHandle::from_raw_handle(pi.hThread.0 as *mut _),
        })
    }
}

/// Receive the IPC handle passed by the parent via `__DD_INTERNAL_PASSED_FD`.
pub fn recv_passed_handle() -> Option<OwnedHandle> {
    let val = env::var(ENV_PASS_FD_KEY).ok()?;
    let handle: u64 = val.parse().ok()?;
    unsafe { Some(OwnedHandle::from_raw_handle(handle as RawHandle)) }
}

fn zeroed_startupinfo() -> STARTUPINFOW {
    STARTUPINFOW {
        cb: 0,
        lpReserved: PWSTR(null_mut()),
        lpDesktop: PWSTR(null_mut()),
        lpTitle: PWSTR(null_mut()),
        dwX: 0,
        dwY: 0,
        dwXSize: 0,
        dwYSize: 0,
        dwXCountChars: 0,
        dwYCountChars: 0,
        dwFillAttribute: 0,
        dwFlags: STARTUPINFOW_FLAGS(0),
        wShowWindow: 0,
        cbReserved2: 0,
        lpReserved2: null_mut(),
        hStdInput: INVALID_HANDLE_VALUE,
        hStdOutput: INVALID_HANDLE_VALUE,
        hStdError: INVALID_HANDLE_VALUE,
    }
}

fn zeroed_process_information() -> PROCESS_INFORMATION {
    PROCESS_INFORMATION {
        hProcess: INVALID_HANDLE_VALUE,
        hThread: INVALID_HANDLE_VALUE,
        dwProcessId: 0,
        dwThreadId: 0,
    }
}

#[allow(clippy::manual_c_str_literals)]
fn open_null(read: bool) -> HANDLE {
    let mut sa = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    HANDLE(unsafe {
        CreateFileA(
            "NUL\0".as_ptr() as LPCSTR,
            if read { GENERIC_READ } else { GENERIC_WRITE },
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &mut sa,
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    } as isize)
}

fn raw_handle_from_stdio(stdio: Stdio, read: bool) -> HANDLE {
    match stdio {
        Stdio::Null => open_null(read),
        Stdio::Handle(h) => HANDLE(h.into_raw_handle() as isize),
    }
}
