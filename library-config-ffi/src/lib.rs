// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod static_config;

use std::path::PathBuf;

use ddcommon_ffi::{self as ffi, slice::AsBytes};
use static_config::{Configurator, LibraryConfig, LibraryConfigName, ProcessInfo};

// TODO: Centos 6 build
// Trust me it works bro 😉😉😉
// #[cfg(linux)]
// std::arch::global_asm!(".symver memcpy,memcpy@GLIBC_2.2.5");

#[no_mangle]
pub extern "C" fn ddog_library_configurator_new(debug_logs: bool) -> Box<Configurator> {
    Box::new(Configurator::new(
        debug_logs,
        PathBuf::from(
            "/etc/datadog-agent/managed/datadog-apm-libraries/stable/libraries_config.yaml",
        ),
    ))
}

#[no_mangle]
/// Sets the path at which we will read the static configuration file.
/// This should mainly be used for testing
pub extern "C" fn ddog_library_configurator_with_path(
    configurator: &mut Configurator,
    p: ffi::CharSlice,
) {
    configurator.static_config_file_path = PathBuf::from(p.to_utf8_lossy().into_owned());
}

#[no_mangle]
pub extern "C" fn ddog_library_configurator_drop(_: Box<Configurator>) {}

#[no_mangle]
pub extern "C" fn ddog_library_configurator_get<'a>(
    configurator: &'a Configurator,
    process_info: ProcessInfo<'a>,
) -> ffi::Result<ffi::Vec<LibraryConfig>> {
    configurator.log_process_info(&process_info);
    configurator
        .get_configuration(process_info)
        .map(ffi::Vec::from_std)
        .into()
}

#[no_mangle]
// In some languages like NodeJS, IO from a shared library is expensive.
// Thus we provide a way to pass the configuration as a byte array instead,
// so that the library can do the IO.
pub extern "C" fn ddog_library_configurator_get_from_bytes<'a>(
    configurator: &'a Configurator,
    process_info: ProcessInfo<'a>,
    config_bytes: ffi::CharSlice<'a>,
) -> ffi::Result<ffi::Vec<LibraryConfig>> {
    configurator.log_process_info(&process_info);
    configurator
        .get_configuration_from_bytes(process_info, config_bytes)
        .map(ffi::Vec::from_std)
        .into()
}

#[no_mangle]
pub extern "C" fn ddog_library_config_name_to_env(name: LibraryConfigName) -> ffi::CStr<'static> {
    ffi::CStr::from_std(name.to_env_name())
}

#[no_mangle]
pub extern "C" fn ddog_library_config_drop(_: ffi::Vec<LibraryConfig>) {}
