// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{ffi::c_char, ops::Deref};

use datadog_library_config::{Configurator, LibraryConfigName, LibraryConfigSource};
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
    path_local: ffi::CharSlice<'a>,
    path_managed: ffi::CharSlice<'a>,
) -> ffi::Result<ffi::Vec<LibraryConfig>> {
    let path_local = path_local.to_utf8_lossy();
    let path_managed = path_managed.to_utf8_lossy();
    let process_info = process_info.ffi_to_rs();
    configurator
        .get_config_from_file(
            path_local.deref().as_ref(),
            path_managed.deref().as_ref(),
            process_info,
        )
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
            Configurator::FLEET_STABLE_CONFIGURATION_PATH.as_ref(),
            Configurator::LOCAL_STABLE_CONFIGURATION_PATH.as_ref(),
            process_info,
        )
        .and_then(LibraryConfig::rs_vec_to_ffi)
        .into()
}

#[no_mangle]
/// Returns a static null-terminated string, containing the name of the environment variable
/// associated with the library configuration
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
/// Returns a static null-terminated string, containing the name of the environment variable
/// associated with the library configuration
pub extern "C" fn ddog_library_config_source_to_string(
    name: LibraryConfigSource,
) -> ffi::CStr<'static> {
    use LibraryConfigSource::*;
    ffi::CStr::from_std(match name {
        LocalFile => ddcommon::cstr!("local_file"),
        Managed => ddcommon::cstr!("managed"),
    })
}

#[no_mangle]
/// Returns a static null-terminated string with the path to the managed stable config yaml config
/// file
pub extern "C" fn ddog_library_config_fleet_stable_config_path() -> ffi::CStr<'static> {
    ffi::CStr::from_std(unsafe {
        let path: &'static str =
            constcat::concat!(Configurator::FLEET_STABLE_CONFIGURATION_PATH, "\0");
        std::ffi::CStr::from_bytes_with_nul_unchecked(path.as_bytes())
    })
}

#[no_mangle]
/// Returns a static null-terminated string with the path to the local stable config yaml config
/// file
pub extern "C" fn ddog_library_config_local_stable_config_path() -> ffi::CStr<'static> {
    ffi::CStr::from_std(unsafe {
        let path: &'static str =
            constcat::concat!(Configurator::LOCAL_STABLE_CONFIGURATION_PATH, "\0");
        std::ffi::CStr::from_bytes_with_nul_unchecked(path.as_bytes())
    })
}

#[no_mangle]
pub extern "C" fn ddog_library_config_drop(_: ffi::Vec<LibraryConfig>) {}
