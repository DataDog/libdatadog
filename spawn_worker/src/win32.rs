// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use kernel32::{CreateFileA, WaitForSingleObject};
use std::ffi::{c_void, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::ptr::null_mut;
use std::{env, fs, io, io::Write};
use winapi::{
    DWORD, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, GENERIC_READ, GENERIC_WRITE, LPCSTR, OPEN_EXISTING, SECURITY_ATTRIBUTES,
    WAIT_OBJECT_0,
};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcess, GetExitCodeProcess, InitializeProcThreadAttributeList,
    UpdateProcThreadAttribute, CREATE_DEFAULT_ERROR_MODE, CREATE_NEW_PROCESS_GROUP,
    CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, STARTUPINFOW_FLAGS,
};
use windows::{
    core::PCSTR,
    Win32::{
        Foundation::{GetLastError, HMODULE},
        System::LibraryLoader::{
            GetModuleFileNameW, GetModuleHandleExA, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        },
    },
};

use crate::{LibDependency, Target, ENV_PASS_FD_KEY};

fn write_trampoline(process_name: &Option<String>) -> io::Result<(PathBuf, File)> {
    let path = if let Some(process_name) = process_name {
        let path = env::temp_dir().join(process_name);

        // Attempt to move it just in case it already exists
        let mut old_path = path.clone();
        old_path.set_extension("old");
        let _ = fs::rename(&path, old_path);

        path
    } else {
        loop {
            let path = env::temp_dir().join(
                std::iter::repeat_with(fastrand::alphanumeric)
                    .take(6)
                    .collect::<String>(),
            );
            if !path.exists() {
                break path;
            }
        }
    };

    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .custom_flags(FILE_ATTRIBUTE_TEMPORARY)
        .open(path.clone())?;

    file.set_len(crate::TRAMPOLINE_BIN.len() as u64)?;
    file.write_all(crate::TRAMPOLINE_BIN)?;
    drop(file);

    // And now open it with FILE_FLAG_DELETE_ON_CLOSE
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(path.clone())?;

    Ok((path, file))
}

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

pub struct SpawnWorker {
    stdin: Option<Stdio>,
    stderr: Option<Stdio>,
    stdout: Option<Stdio>,
    target: Target,
    env: Vec<(OsString, OsString)>,
    process_name: Option<String>,
    passed_handle: Option<OwnedHandle>,
    shared_lib_dependencies: Vec<LibDependency>,
}

impl Default for SpawnWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl SpawnWorker {
    pub fn new() -> Self {
        Self {
            stdin: None,
            stdout: None,
            stderr: None,
            target: Target::Noop,
            env: env::vars_os().collect(),
            process_name: None,
            passed_handle: None,
            shared_lib_dependencies: vec![],
        }
    }

    pub fn shared_lib_dependencies(&mut self, deps: Vec<LibDependency>) -> &mut Self {
        self.shared_lib_dependencies = deps;
        self
    }

    pub fn target<T: Into<Target>>(&mut self, target: T) -> &mut Self {
        self.target = target.into();
        self
    }

    pub fn stdin<T: Into<Stdio>>(&mut self, stdio: T) -> &mut Self {
        self.stdin = Some(stdio.into());
        self
    }

    pub fn stdout<T: Into<Stdio>>(&mut self, stdio: T) -> &mut Self {
        self.stdout = Some(stdio.into());
        self
    }

    pub fn stderr<T: Into<Stdio>>(&mut self, stdio: T) -> &mut Self {
        self.stderr = Some(stdio.into());
        self
    }

    pub fn pass_handle(&mut self, handle: OwnedHandle) -> &mut Self {
        self.passed_handle = Some(handle);
        self
    }

    pub fn append_env<K: Into<OsString>, V: Into<OsString>>(
        &mut self,
        key: K,
        value: V,
    ) -> &mut Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn process_name<S: Into<String>>(&mut self, process_name: S) -> &mut Self {
        self.process_name = Some(process_name.into());
        self
    }

    pub fn wait_spawn(&mut self) -> anyhow::Result<()> {
        self.spawn()?;
        Ok(())
    }

    pub fn spawn(&mut self) -> anyhow::Result<Child> {
        self.do_spawn()
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

    #[allow(clippy::manual_c_str_literals)] // c"NUL" from 1.77 and up
    fn open_null(read: bool) -> HANDLE {
        let mut sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
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
            Stdio::Null => Self::open_null(read),
            Stdio::Handle(handle) => HANDLE(handle.into_raw_handle() as isize),
        }
    }

    fn do_spawn(&mut self) -> anyhow::Result<Child> {
        let (path, _file) = write_trampoline(&self.process_name)?;

        let mut envs = self.env.clone();
        let mut inherited_handles = vec![];

        let mut args = vec![];
        args.push("".to_string());

        let entrypoint_symbol_name = match &self.target {
            Target::Entrypoint(f) => {
                let path = get_trampoline_target_data(f.ptr as *const u8)?;
                args.push(path);
                f.symbol_name.to_string_lossy().into_owned()
            }
            Target::ManualTrampoline(path, symbol_name) => {
                args.push(path.clone());
                symbol_name.clone()
            }
            Target::Noop => todo!(),
        };

        for dep in &self.shared_lib_dependencies {
            match dep {
                LibDependency::Path(path) => {
                    args.push(path.to_string_lossy().into_owned());
                }
            }
        }

        args.push(entrypoint_symbol_name);

        if let Some(ref handle) = self.passed_handle {
            envs.push((
                ENV_PASS_FD_KEY.parse().unwrap(),
                (handle.as_raw_handle() as u64).to_string().parse().unwrap(),
            ));
            inherited_handles.push(HANDLE(handle.as_raw_handle() as isize));
        }

        let (stdin_val, stdout_val, stderr_val) =
            (self.stdin.take(), self.stdout.take(), self.stderr.take());
        let stdin = Self::raw_handle_from_stdio(stdin_val.unwrap_or(Stdio::Null), true);
        let stdout = Self::raw_handle_from_stdio(stdout_val.unwrap_or(Stdio::Null), false);
        let stderr = Self::raw_handle_from_stdio(stderr_val.unwrap_or(Stdio::Null), false);

        inherited_handles.push(stdin);
        inherited_handles.push(stdout);
        inherited_handles.push(stderr);

        let mut size: usize = 0;
        let _ = unsafe {
            InitializeProcThreadAttributeList(
                LPPROC_THREAD_ATTRIBUTE_LIST(null_mut()),
                1,
                0,
                &mut size,
            )
        };
        let mut attribute_list_vec: Vec<u8> = Vec::with_capacity(size);
        let attribute_list =
            LPPROC_THREAD_ATTRIBUTE_LIST(attribute_list_vec.as_mut_ptr() as *mut c_void);
        unsafe { InitializeProcThreadAttributeList(attribute_list, 1, 0, &mut size).unwrap() };
        unsafe {
            UpdateProcThreadAttribute(
                attribute_list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(inherited_handles.as_mut_ptr() as *mut c_void),
                inherited_handles.len() * std::mem::size_of::<HANDLE>(),
                None,
                None,
            )
        }
        .unwrap();

        let mut pi = Self::zeroed_process_information();
        let mut si = STARTUPINFOEXW {
            StartupInfo: Self::zeroed_startupinfo(),
            lpAttributeList: attribute_list,
        };
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = stdin;
        si.StartupInfo.hStdOutput = stdout;
        si.StartupInfo.hStdError = stderr;

        let mut cmd: Vec<u16> = Vec::new();
        cmd.push(b'"' as u16);
        cmd.extend(path.as_os_str().encode_wide());
        cmd.push(b'"' as u16);

        for arg in &args {
            cmd.push(' ' as u16);
            cmd.push(b'"' as u16);
            // We don't have special chars in our args, so avoid extra quoting
            cmd.extend(OsStr::new(arg.as_str()).encode_wide());
            cmd.push(b'"' as u16);
        }
        cmd.push(0);

        let mut envp: Vec<u16> = Vec::new();

        for (key, val) in envs {
            envp.extend(key.encode_wide());
            envp.push('=' as u16);
            envp.extend(val.encode_wide());
            envp.push(0);
        }
        envp.push(0);

        let mut program: Vec<u16> = Vec::new();
        program.extend(path.as_os_str().encode_wide());
        program.push(0);

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
                Some(envp.as_mut_ptr() as *mut c_void),
                PCWSTR::null(),
                &si.StartupInfo,
                &mut pi,
            )
        }
        .map_err(|e| {
            let e: anyhow::Error = e.into();
            e.context(format!(
                "Tried to spawn {} with args {}",
                path.display(),
                args.join(", ")
            ))
        })?;

        unsafe {
            Ok(Child {
                handle: OwnedHandle::from_raw_handle(pi.hProcess.to_owned().0 as *mut c_void),
                main_thread_handle: OwnedHandle::from_raw_handle(
                    pi.hThread.to_owned().0 as *mut c_void,
                ),
            })
        }
    }
}

pub fn recv_passed_handle() -> Option<OwnedHandle> {
    let val = env::var(ENV_PASS_FD_KEY).ok()?;
    let handle: u64 = val.parse().ok()?;
    unsafe { Some(OwnedHandle::from_raw_handle(handle as RawHandle)) }
}

fn get_module_file_name(h: HMODULE) -> anyhow::Result<String> {
    let mut buf = vec![0; 2000];
    loop {
        let read: usize = unsafe { GetModuleFileNameW(h, &mut buf) } as usize;
        if read == 0 {
            unsafe { GetLastError() }?;
            anyhow::bail!("unknown error getting module name");
        }

        if read == buf.len() {
            buf.resize(buf.len() * 2, 0)
        } else {
            return Ok(String::from_utf16(&buf[0..read])?);
        }
    }
}

pub fn get_trampoline_target_data(f: *const u8) -> anyhow::Result<String> {
    let mut h = HMODULE::default();

    unsafe {
        GetModuleHandleExA(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCSTR::from_raw(f),
            &mut h as *mut HMODULE,
        )?
    };

    get_module_file_name(h)
}

pub struct Child {
    pub handle: OwnedHandle,
    pub main_thread_handle: OwnedHandle,
}

impl Child {
    pub fn wait(&self) -> io::Result<ExitStatus> {
        unsafe {
            let res = WaitForSingleObject(self.handle.as_raw_handle(), INFINITE);
            let mut status = 0;
            if res != WAIT_OBJECT_0 {
                return Err(io::Error::last_os_error());
            }
            GetExitCodeProcess(HANDLE(self.handle.as_raw_handle() as isize), &mut status)?;
            Ok(ExitStatus::from_raw(status))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::get_trampoline_target_data;

    #[test]
    pub fn test_trampoline_target_data() {
        let path = get_trampoline_target_data(test_trampoline_target_data as *const u8).unwrap();

        let current_exe = env::current_exe().unwrap().to_str().unwrap().to_owned();
        assert_eq!(current_exe, path);
    }
}
