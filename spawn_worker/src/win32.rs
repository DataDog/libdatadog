// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{
    io::{Seek, Write},
    mem,
    process::{Child, Command, Stdio},
};

pub(crate) fn write_trampoline() -> anyhow::Result<tempfile::NamedTempFile> {
    let tmp_file = tempfile::NamedTempFile::new()?;
    let mut file = tmp_file.as_file();
    file.set_len(crate::TRAMPOLINE_BIN.len() as u64)?;
    file.write_all(crate::TRAMPOLINE_BIN)?;
    file.rewind()?;

    Ok(tmp_file)
}

use windows::{
    core::{PCSTR, PCWSTR},
    Win32::{
        Foundation::{GetLastError, HMODULE},
        System::{
            Diagnostics::Debug::{
                SymFromAddrW, SymInitializeW, MAX_SYM_NAME, SYMBOL_INFOW, SYMBOL_INFO_PACKAGEW,
            },
            LibraryLoader::{
                GetModuleFileNameW, GetModuleHandleExA, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            },
            Threading::GetCurrentProcess,
        },
    },
};

pub enum Target {
    Entrypoint(crate::Entrypoint),
    ManualTrampoline(String, String),
    Noop,
}

pub struct SpawnWorker {
    stdin: Option<Stdio>,
    stderr: Option<Stdio>,
    stdout: Option<Stdio>,
    target: Target,
    env_clear: bool,
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

    pub fn spawn(&mut self) -> anyhow::Result<Child> {
        self.do_spawn()
    }

    fn do_spawn(&mut self) -> anyhow::Result<Child> {
        let (f, path) = write_trampoline()?.keep()?;
        drop(f);

        let mut cmd = Command::new(path);

        if let Some(stdin) = self.stdin.take() {
            cmd.stdin(stdin);
        }
        if let Some(stdout) = self.stdout.take() {
            cmd.stdout(stdout);
        }

        if let Some(stderr) = self.stderr.take() {
            cmd.stderr(stderr);
        }

        // if self.env_clear {
        //     cmd.env_clear();
        // }

        match &self.target {
            Target::Entrypoint(f) => {
                let (path, symbol_name) = get_trampoline_target_data(f.symbol_name.as_ptr() as *const u8)?;
                cmd.args([path, symbol_name])
            }
            Target::ManualTrampoline(path, symbol_name) => cmd.args([path, symbol_name]),
            Target::Noop => todo!(),
        };

        Ok(cmd.spawn()?)
    }
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

fn get_sym_name(f: *const u8) -> anyhow::Result<String> {
    let hprocess = unsafe { GetCurrentProcess() };

    unsafe { SymInitializeW(hprocess, PCWSTR::null(), true).ok()? };

    let mut sym = SYMBOL_INFO_PACKAGEW::default();
    sym.si.SizeOfStruct = mem::size_of::<SYMBOL_INFOW>() as u32;
    sym.si.MaxNameLen = sym.name.len() as u32;
    unsafe {
        SymFromAddrW(hprocess, f as u64, None, &mut sym.si as *mut SYMBOL_INFOW)
            .ok()
            .unwrap();
    }
    let sn_len = sym.si.NameLen as usize;
    let reassembled_name =
        unsafe { mem::transmute::<_, &[u16; MAX_SYM_NAME as usize]>(&sym.si.Name) };

    Ok(String::from_utf16(&reassembled_name[0..sn_len])?)
}

pub fn get_trampoline_target_data(f: *const u8) -> anyhow::Result<(String, String)> {
    let mut h = HMODULE::default();

    unsafe {
        GetModuleHandleExA(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCSTR::from_raw(f),
            &mut h as *mut HMODULE,
        )
        .ok()?
    };
    eprint!("a: {:?}", h);

    let module_file_name = get_module_file_name(h)?;

    let fn_name = get_sym_name(f)?;

    Ok((module_file_name, fn_name))
}

#[cfg(test)]
mod tests {
    use std::{env};

    

    use super::get_trampoline_target_data;

    #[test]
    pub fn test_trampoline_target_data() {
        let (path, name) =
            get_trampoline_target_data(test_trampoline_target_data as *const u8).unwrap();

        let current_exe = env::current_exe().unwrap().to_str().unwrap().to_owned();
        assert_eq!(current_exe, path);

        assert!(name.ends_with("test_trampoline_target_data"));
    }
}
