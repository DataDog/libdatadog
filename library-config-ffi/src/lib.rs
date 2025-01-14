// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{ffi::c_char, ops::Deref};

use datadog_library_config::{Configurator, LibraryConfigName};
use ddcommon_ffi::{self as ffi, slice::AsBytes, Slice};

// TODO: Centos 6 build
// Trust me it works bro 😉😉😉
// #[cfg(linux)]
// std::arch::global_asm!(".symver memcpy,memcpy@GLIBC_2.2.5");

#[repr(C)]
pub struct ProcessInfo<'a> {
    pub args: ffi::Slice<'a, ffi::CharSlice<'a>>,
    pub envp: ffi::Slice<'a, ffi::CharSlice<'a>>,
    pub language: ffi::CharSlice<'a>,
}

fn cast_slice_of_slice_ref<'a, 'b>(
    s: &'b ffi::Slice<'a, ffi::CharSlice<'a>>,
) -> &'b ffi::Slice<'a, ffi::slice::ByteSlice<'a>> {
    // Safety:
    // ffi::CharSlice and ffi::slice::ByteSlice have the same layout, and since they are wrappers
    // around *const char i8 and *const char u8 respectively, they are trivially interchangeable
    unsafe { std::mem::transmute(s) }
}

impl<'a> ProcessInfo<'a> {
    fn ffi_to_rs(&'a self) -> datadog_library_config::ProcessInfo<'a, ffi::slice::ByteSlice<'a>> {
        let language =
            unsafe { std::mem::transmute_copy::<Slice<'a, c_char>, Slice<'a, u8>>(&self.language) };
        datadog_library_config::ProcessInfo::<Slice<'a, u8>> {
            args: cast_slice_of_slice_ref(&self.args),
            envp: cast_slice_of_slice_ref(&self.envp),
            language,
        }
    }
}

#[repr(C)]
pub struct LibraryConfig {
    pub name: LibraryConfigName,
    pub value: ffi::CString,
}

impl LibraryConfig {
    fn rs_vec_to_ffi(
        configs: Vec<datadog_library_config::LibraryConfig>,
    ) -> anyhow::Result<ffi::Vec<Self>> {
        let cfg: Vec<LibraryConfig> = configs
            .into_iter()
            .map(|c| {
                Ok(LibraryConfig {
                    name: c.name,
                    value: ffi::CString::from_std(std::ffi::CString::new(c.value)?),
                })
            })
            .collect::<Result<Vec<_>, std::ffi::NulError>>()?;
        Ok(ffi::Vec::from_std(cfg))
    }
}

#[no_mangle]
pub extern "C" fn ddog_library_configurator_new(debug_logs: bool) -> Box<Configurator> {
    Box::new(Configurator::new(debug_logs))
}

#[no_mangle]
pub extern "C" fn ddog_library_configurator_drop(_: Box<Configurator>) {}

#[no_mangle]
pub extern "C" fn ddog_library_configurator_get_path<'a>(
    configurator: &'a Configurator,
    process_info: ProcessInfo<'a>,
    path: ffi::CharSlice<'a>,
) -> ffi::Result<ffi::Vec<LibraryConfig>> {
    let path = path.to_utf8_lossy();
    let process_info = process_info.ffi_to_rs();
    configurator
        .get_config_from_file(path.deref().as_ref(), process_info)
        .and_then(LibraryConfig::rs_vec_to_ffi)
        .into()
}

#[no_mangle]
pub extern "C" fn ddog_library_configurator_get<'a>(
    configurator: &'a Configurator,
    process_info: ProcessInfo<'a>,
) -> ffi::Result<ffi::Vec<LibraryConfig>> {
    let process_info = process_info.ffi_to_rs();
    configurator
        .get_config_from_file(
            "/etc/datadog-agent/managed/datadog-apm-libraries/stable/libraries_config.yaml"
                .as_ref(),
            process_info,
        )
        .and_then(LibraryConfig::rs_vec_to_ffi)
        .into()
}

#[no_mangle]
// In some languages like NodeJS, IO from a shared library is expensive.
// Thus we provide a way to pass the configuration as a byte array instead,
// so that the library can do the IO.
pub extern "C" fn ddog_library_configurator_get_from_bytes<'a>(
    configurator: &'a Configurator,
    process_info: ProcessInfo<'a>,
    config_bytes: ffi::slice::ByteSlice<'a>,
) -> ffi::Result<ffi::Vec<LibraryConfig>> {
    let process_info = process_info.ffi_to_rs();
    configurator
        .get_config_from_bytes(&config_bytes, process_info)
        .and_then(LibraryConfig::rs_vec_to_ffi)
        .into()
}

#[no_mangle]
pub extern "C" fn ddog_library_config_name_to_env(name: LibraryConfigName) -> ffi::CStr<'static> {
    use LibraryConfigName::*;
    ffi::CStr::from_std(match name {
        DdTraceDebug => ddcommon::cstr!("DD_TRACE_DEBUG"),
        DdService => ddcommon::cstr!("DD_SERVICE"),
        DdEnv => ddcommon::cstr!("DD_ENV"),
        DdVersion => ddcommon::cstr!("DD_VERSION"),
        DdProfilingEnabled => ddcommon::cstr!("DD_PROFILING_ENABLED"),
    })
}

#[no_mangle]
pub extern "C" fn ddog_library_config_drop(_: ffi::Vec<LibraryConfig>) {}
