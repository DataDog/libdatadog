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
        PathBuf::from("/etc/datadog-agent/managed/datadog-apm-libraries/static_config.yaml"),
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
    if configurator.debug_logs {
        eprintln!("Called library_config_common_component:");
        eprintln!("\tconfigurator: {:?}", configurator);
        eprintln!("\tprocess args: {:?}", process_info.args);
        // TODO: this is for testing purpose, we don't want to log env variables
        eprintln!("\tprocess envs: {:?}", process_info.args);
    }
    configurator
        .get_configuration(process_info)
        .map(ffi::Vec::from_std)
        .into()
}

#[no_mangle]
pub extern "C" fn ddog_library_config_name_to_env(name: LibraryConfigName) -> ffi::CStr<'static> {
    ffi::CStr::from_std(name.to_env_name())
}

#[no_mangle]
pub extern "C" fn ddog_library_config_drop(_: ffi::Vec<LibraryConfig>) {}
