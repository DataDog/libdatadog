// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::ffi::OsString;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::os::windows::process::CommandExt;
use std::{
    env,
    io::{Seek, Write},
    process::{Child, Command, Stdio},
};

pub(crate) fn write_trampoline(
    process_name: &Option<String>,
) -> anyhow::Result<tempfile::NamedTempFile> {
    let tmp_file = if let Some(process_name) = process_name {
        tempfile::Builder::new()
            .prefix(&format!("{}-", process_name))
            .tempfile()
    } else {
        tempfile::NamedTempFile::new()
    }?;
    let mut file = tmp_file.as_file();
    file.set_len(crate::TRAMPOLINE_BIN.len() as u64)?;
    file.write_all(crate::TRAMPOLINE_BIN)?;
    file.rewind()?;

    Ok(tmp_file)
}

use windows::Win32::System::Threading::{
    CREATE_DEFAULT_ERROR_MODE, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
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

use crate::{Target, ENV_PASS_FD_KEY};

pub struct SpawnWorker {
    stdin: Option<Stdio>,
    stderr: Option<Stdio>,
    stdout: Option<Stdio>,
    target: Target,
    env_clear: bool,
    env: Vec<(OsString, OsString)>,
    process_name: Option<String>,
    passed_handle: Option<OwnedHandle>,
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
            env_clear: false,
            env: Vec::new(),
            process_name: None,
            passed_handle: None,
        }
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

    fn do_spawn(&mut self) -> anyhow::Result<Child> {
        let (f, path) = write_trampoline(&self.process_name)?.keep()?;
        drop(f);

        let mut cmd = Command::new(path);
        cmd.creation_flags(
            DETACHED_PROCESS.0 | CREATE_NEW_PROCESS_GROUP.0 | CREATE_DEFAULT_ERROR_MODE.0,
        );

        if let Some(stdin) = self.stdin.take() {
            cmd.stdin(stdin);
        }
        if let Some(stdout) = self.stdout.take() {
            cmd.stdout(stdout);
        }

        if let Some(stderr) = self.stderr.take() {
            cmd.stderr(stderr);
        }

        if self.env_clear {
            cmd.env_clear();
        }

        for (key, val) in self.env.iter() {
            cmd.env(key, val);
        }

        if let Some(ref handle) = self.passed_handle {
            cmd.env(ENV_PASS_FD_KEY, (handle.as_raw_handle() as u64).to_string());
        }

        cmd.arg("");

        match &self.target {
            Target::Entrypoint(f) => {
                let path = get_trampoline_target_data(f.ptr as *const u8)?;
                cmd.args([path, f.symbol_name.to_string_lossy().into_owned()])
            }
            Target::ManualTrampoline(path, symbol_name) => cmd.args([path, symbol_name]),
            Target::Noop => todo!(),
        };

        Ok(cmd.spawn()?)
    }
}

pub fn recv_passed_handle() -> Option<OwnedHandle> {
    let val = env::var(ENV_PASS_FD_KEY).ok()?;
    let handle: u64 = val.parse().ok()?;
    unsafe { Some(OwnedHandle::from_raw_handle(handle as RawHandle)) }
}

fn get_module_file_name(h: HMODULE) -> anyhow::Result<String> {
    let mut buf = Vec::new();
    buf.resize(2000, 0);
    loop {
        let read: usize = unsafe { GetModuleFileNameW(h, &mut buf) } as usize;
        if read == 0 {
            return Err(unsafe { GetLastError() }
                .ok()
                .err()
                .map(|e| e.into())
                .unwrap_or_else(|| anyhow::anyhow!("unknown error getting module name")));
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
        )
        .ok()?
    };

    get_module_file_name(h)
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
