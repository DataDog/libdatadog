// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_library_config::{self as lib_config, LibraryConfigSource};
use ddcommon_ffi::{self as ffi, slice::AsBytes, CharSlice};

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

impl<'a> ProcessInfo<'a> {
    fn ffi_to_rs(&'a self) -> lib_config::ProcessInfo {
        lib_config::ProcessInfo {
            args: self.args.iter().map(|e| e.as_bytes().to_vec()).collect(),
            envp: self.envp.iter().map(|e| e.as_bytes().to_vec()).collect(),
            language: self.language.as_bytes().to_vec(),
        }
    }
}

#[repr(C)]
pub struct LibraryConfig {
    pub name: ffi::CString,
    pub value: ffi::CString,
    pub source: LibraryConfigSource,
    pub config_id: ffi::CString,
}

impl LibraryConfig {
    fn rs_vec_to_ffi(configs: Vec<lib_config::LibraryConfig>) -> anyhow::Result<ffi::Vec<Self>> {
        let cfg: Vec<LibraryConfig> = configs
            .into_iter()
            .map(|c| {
                Ok(LibraryConfig {
                    name: ffi::CString::from_std(std::ffi::CString::new(c.name)?),
                    value: ffi::CString::from_std(std::ffi::CString::new(c.value)?),
                    source: c.source,
                    config_id: ffi::CString::from_std(std::ffi::CString::new(
                        c.config_id.unwrap_or_default(),
                    )?),
                })
            })
            .collect::<Result<Vec<_>, std::ffi::NulError>>()?;
        Ok(ffi::Vec::from_std(cfg))
    }
}

pub struct Configurator<'a> {
    inner: lib_config::Configurator,
    language: CharSlice<'a>,
    fleet_path: Option<ffi::CStr<'a>>,
    local_path: Option<ffi::CStr<'a>>,
    process_info: Option<lib_config::ProcessInfo>,
}

// type FfiConfigurator<'a>  = Configurator<'a>;

#[no_mangle]
pub extern "C" fn ddog_library_configurator_new(
    debug_logs: bool,
    language: CharSlice,
) -> Box<Configurator> {
    Box::new(Configurator {
        inner: lib_config::Configurator::new(debug_logs),
        language,
        fleet_path: None,
        local_path: None,
        process_info: None,
    })
}

#[no_mangle]
pub extern "C" fn ddog_library_configurator_with_local_path<'a>(
    c: &mut Configurator<'a>,
    local_path: ffi::CStr<'a>,
) {
    c.local_path = Some(local_path);
}

#[no_mangle]
pub extern "C" fn ddog_library_configurator_with_fleet_path<'a>(
    c: &mut Configurator<'a>,
    local_path: ffi::CStr<'a>,
) {
    c.fleet_path = Some(local_path);
}

#[no_mangle]
pub extern "C" fn ddog_library_configurator_with_process_info<'a>(
    c: &mut Configurator<'a>,
    p: ProcessInfo<'a>,
) {
    c.process_info = Some(p.ffi_to_rs());
}

#[no_mangle]
pub extern "C" fn ddog_library_configurator_with_detect_process_info(c: &mut Configurator) {
    c.process_info = Some(lib_config::ProcessInfo::detect_global(
        c.language.to_utf8_lossy().into_owned(),
    ));
}

#[no_mangle]
pub extern "C" fn ddog_library_configurator_drop(_: Box<Configurator>) {}

#[no_mangle]
pub extern "C" fn ddog_library_configurator_get(
    configurator: &Configurator,
) -> ffi::Result<ffi::Vec<LibraryConfig>> {
    (|| {
        let local_path = configurator
            .local_path
            .as_ref()
            .map(|p| p.into_std().to_str())
            .transpose()?
            .unwrap_or(lib_config::Configurator::LOCAL_STABLE_CONFIGURATION_PATH);
        let fleet_path = configurator
            .fleet_path
            .as_ref()
            .map(|p| p.into_std().to_str())
            .transpose()?
            .unwrap_or(lib_config::Configurator::FLEET_STABLE_CONFIGURATION_PATH);
        let detected_process_info;
        let process_info = match configurator.process_info {
            Some(ref p) => p,
            None => {
                detected_process_info = lib_config::ProcessInfo::detect_global(
                    configurator.language.to_utf8_lossy().into_owned(),
                );
                &detected_process_info
            }
        };

        configurator.inner.get_config_from_file(
            local_path.as_ref(),
            fleet_path.as_ref(),
            process_info,
        )
    })()
    .and_then(LibraryConfig::rs_vec_to_ffi)
    .into()
}

#[no_mangle]
/// Returns a static null-terminated string, containing the name of the environment variable
/// associated with the library configuration
pub extern "C" fn ddog_library_config_source_to_string(
    name: LibraryConfigSource,
) -> ffi::CStr<'static> {
    ffi::CStr::from_std(match name {
        LibraryConfigSource::LocalStableConfig => ddcommon::cstr!("local_stable_config"),
        LibraryConfigSource::FleetStableConfig => ddcommon::cstr!("fleet_stable_config"),
    })
}

#[no_mangle]
/// Returns a static null-terminated string with the path to the managed stable config yaml config
/// file
pub extern "C" fn ddog_library_config_fleet_stable_config_path() -> ffi::CStr<'static> {
    ffi::CStr::from_std(unsafe {
        let path: &'static str = constcat::concat!(
            lib_config::Configurator::FLEET_STABLE_CONFIGURATION_PATH,
            "\0"
        );
        std::ffi::CStr::from_bytes_with_nul_unchecked(path.as_bytes())
    })
}

#[no_mangle]
/// Returns a static null-terminated string with the path to the local stable config yaml config
/// file
pub extern "C" fn ddog_library_config_local_stable_config_path() -> ffi::CStr<'static> {
    ffi::CStr::from_std(unsafe {
        let path: &'static str = constcat::concat!(
            lib_config::Configurator::LOCAL_STABLE_CONFIGURATION_PATH,
            "\0"
        );
        std::ffi::CStr::from_bytes_with_nul_unchecked(path.as_bytes())
    })
}

#[no_mangle]
pub extern "C" fn ddog_library_config_drop(_: ffi::Vec<LibraryConfig>) {}
