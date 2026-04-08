// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
// Only set #![no_std] when `no_std_entry` is active. When built as a `lib` crate-type (not a
// standalone staticlib/cdylib), the consuming crate provides its own allocator and panic handler,
// so #![no_std] should only be set when this crate is the binary entry point.
#![cfg_attr(all(not(feature = "std"), feature = "no_std_entry"), no_std)]

extern crate alloc;

#[cfg(all(not(feature = "std"), feature = "no_std_entry", not(panic = "abort")))]
compile_error!(
    "The `no_std_entry` feature requires `panic = \"abort\"` in the Cargo profile. \
     Building with panic=unwind causes undefined behavior at FFI boundaries."
);

#[cfg(all(not(feature = "std"), feature = "no_std_entry"))]
mod no_std_support {
    #[cfg(target_os = "linux")]
    #[global_allocator]
    static ALLOC: rustix_dlmalloc::GlobalDlmalloc = rustix_dlmalloc::GlobalDlmalloc;

    #[cfg(not(target_os = "linux"))]
    #[global_allocator]
    static ALLOC: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo) -> ! {
        // abort() is provided by the C runtime, which is always linked for FFI libs.
        // Note: _info is intentionally discarded — in no_std there is no reliable way to
        // write diagnostics without std I/O. Panics in no_std mode are silent and fatal.
        extern "C" {
            fn abort() -> !;
        }
        // SAFETY: abort() is a C standard library function with no preconditions; it
        // unconditionally terminates the process.
        unsafe { abort() }
    }

    /// Required by the Rust compiler's exception handling ABI. A no-op is safe because
    /// unwinding will never occur under `panic = "abort"` (enforced by the compile_error!
    /// guard above). WARNING: this symbol is globally visible — this library must not be
    /// linked with other Rust code compiled with `panic = "unwind"`.
    #[no_mangle]
    pub extern "C" fn rust_eh_personality() {}
}

#[cfg(feature = "std")]
pub mod tracer_metadata;

use libdd_common_ffi::{self as ffi, slice::AsBytes};
#[cfg(feature = "std")]
use libdd_common_ffi::{CString, Error};

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::ToString, vec::Vec};

use ffi::CharSlice;
use libdd_library_config::{self as lib_config, LibraryConfigSource};

#[cfg(all(feature = "std", feature = "catch_panic", panic = "unwind"))]
use std::panic::{catch_unwind, AssertUnwindSafe};

#[cfg(all(feature = "std", feature = "catch_panic", panic = "unwind"))]
macro_rules! catch_panic {
    ($f:expr, $err_ctor:expr) => {
        match catch_unwind(AssertUnwindSafe(|| $f)) {
            Ok(ret) => ret,
            Err(info) => {
                let panic_msg = if let Some(s) = info.downcast_ref::<&'static str>() {
                    s.to_string()
                } else if let Some(s) = info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "FFI function panicked".to_string()
                };
                $err_ctor(panic_msg)
            }
        }
    };
}

#[cfg(all(feature = "std", any(not(feature = "catch_panic"), panic = "abort")))]
macro_rules! catch_panic {
    ($f:expr, $err_ctor:expr) => {
        $f
    };
}

#[cfg(not(feature = "std"))]
macro_rules! catch_panic {
    ($f:expr, $err_ctor:expr) => {
        $f
    };
}

#[cfg(feature = "std")]
/// A result type that includes debug/log messages along with the data
#[repr(C)]
pub struct OkResult {
    pub value: ffi::Vec<LibraryConfig>,
    pub logs: CString,
}

#[cfg(feature = "std")]
#[repr(C)]
pub enum LibraryConfigLoggedResult {
    Ok(OkResult),
    Err(Error),
}

// TODO: Centos 6 build
// Trust me it works bro
// #[cfg(linux)]
// std::arch::global_asm!(".symver memcpy,memcpy@GLIBC_2.2.5");

#[repr(C)]
pub struct ProcessInfo<'a> {
    pub args: ffi::Slice<'a, CharSlice<'a>>,
    pub envp: ffi::Slice<'a, CharSlice<'a>>,
    pub language: CharSlice<'a>,
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
    fn vec_to_ffi(
        configs: Vec<lib_config::LibraryConfig>,
    ) -> Result<ffi::Vec<Self>, alloc::ffi::NulError> {
        let cfg: Vec<LibraryConfig> = configs
            .into_iter()
            .map(|c| {
                Ok(LibraryConfig {
                    name: ffi::CString::new(c.name)?,
                    value: ffi::CString::new(c.value)?,
                    source: c.source,
                    config_id: ffi::CString::new(c.config_id.unwrap_or_default())?,
                })
            })
            .collect::<Result<Vec<_>, alloc::ffi::NulError>>()?;
        Ok(ffi::Vec::from_std(cfg))
    }

    #[cfg(feature = "std")]
    fn logged_result_to_ffi_with_messages(
        result: libdd_library_config::LoggedResult<Vec<lib_config::LibraryConfig>, anyhow::Error>,
    ) -> LibraryConfigLoggedResult {
        match result {
            libdd_library_config::LoggedResult::Ok(configs, logs) => {
                match Self::vec_to_ffi(configs) {
                    Ok(ffi_configs) => {
                        let messages = logs.join("\n");
                        let cstring_logs = CString::new_or_empty(messages);
                        LibraryConfigLoggedResult::Ok(OkResult {
                            value: ffi_configs,
                            logs: cstring_logs,
                        })
                    }
                    Err(err) => LibraryConfigLoggedResult::Err(Error::from(err.to_string())),
                }
            }
            libdd_library_config::LoggedResult::Err(err) => {
                LibraryConfigLoggedResult::Err(err.into())
            }
        }
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

#[cfg(feature = "std")]
#[no_mangle]
pub extern "C" fn ddog_library_configurator_with_detect_process_info(c: &mut Configurator) {
    c.process_info = Some(lib_config::ProcessInfo::detect_global(
        c.language.to_utf8_lossy().into_owned(),
    ));
}

#[no_mangle]
pub extern "C" fn ddog_library_configurator_drop(_: Box<Configurator>) {}

/// Result type for [`ddog_library_configurator_get_from_bytes`]. Available in both std and no_std.
#[repr(C)]
pub enum LibraryConfigBytesResult {
    Ok(ffi::Vec<LibraryConfig>),
    Err(ffi::CString),
}

/// Parses library configuration from raw YAML bytes (local and fleet configs).
///
/// `process_info` must be set on the configurator before calling this function
/// (via `ddog_library_configurator_with_process_info`).
///
/// Available in both std and no_std builds (unlike `ddog_library_configurator_get` which
/// reads files from disk and requires std).
#[no_mangle]
pub extern "C" fn ddog_library_configurator_get_from_bytes(
    configurator: &Configurator,
    local_config_bytes: CharSlice,
    fleet_config_bytes: CharSlice,
) -> LibraryConfigBytesResult {
    catch_panic!(
        {
            let process_info = match configurator.process_info {
                Some(ref p) => p,
                None => {
                    return LibraryConfigBytesResult::Err(ffi::CString::new_or_empty(
                        "process_info must be set before calling get_from_bytes",
                    ));
                }
            };

            let result = configurator.inner.get_config_from_bytes(
                local_config_bytes.as_bytes(),
                fleet_config_bytes.as_bytes(),
                process_info,
            );

            match result {
                Ok(configs) => match LibraryConfig::vec_to_ffi(configs) {
                    Ok(ffi_configs) => LibraryConfigBytesResult::Ok(ffi_configs),
                    Err(e) => {
                        LibraryConfigBytesResult::Err(ffi::CString::new_or_empty(e.to_string()))
                    }
                },
                Err(e) => {
                    LibraryConfigBytesResult::Err(ffi::CString::new_or_empty(e.to_string()))
                }
            }
        },
        |msg| LibraryConfigBytesResult::Err(ffi::CString::new_or_empty(msg))
    )
}

#[no_mangle]
pub extern "C" fn ddog_library_config_bytes_result_drop(_: LibraryConfigBytesResult) {}

#[cfg(feature = "std")]
#[no_mangle]
pub extern "C" fn ddog_library_configurator_get(
    configurator: &Configurator,
) -> LibraryConfigLoggedResult {
    catch_panic!(
        {
            let local_path = configurator
                .local_path
                .as_ref()
                .and_then(|p| p.into_std().to_str().ok())
                .unwrap_or(lib_config::Configurator::LOCAL_STABLE_CONFIGURATION_PATH);
            let fleet_path = configurator
                .fleet_path
                .as_ref()
                .and_then(|p| p.into_std().to_str().ok())
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

            let result = configurator.inner.get_config_from_file(
                local_path.as_ref(),
                fleet_path.as_ref(),
                process_info,
            );

            LibraryConfig::logged_result_to_ffi_with_messages(result)
        },
        |msg| LibraryConfigLoggedResult::Err(Error::from(format!("FFI function panicked: {msg}")))
    )
}

#[cfg(feature = "std")]
#[no_mangle]
/// Returns a static null-terminated string, containing the name of the environment variable
/// associated with the library configuration
pub extern "C" fn ddog_library_config_source_to_string(
    name: LibraryConfigSource,
) -> ffi::CStr<'static> {
    ffi::CStr::from_std(match name {
        LibraryConfigSource::LocalStableConfig => libdd_common::cstr!("local_stable_config"),
        LibraryConfigSource::FleetStableConfig => libdd_common::cstr!("fleet_stable_config"),
    })
}

#[cfg(feature = "std")]
#[no_mangle]
/// Returns a static null-terminated string with the path to the managed stable config yaml config
/// file
pub extern "C" fn ddog_library_config_fleet_stable_config_path() -> ffi::CStr<'static> {
    // SAFETY: constcat! appends a literal "\0", guaranteeing a single null terminator
    // at the end. The path constant contains no interior null bytes.
    ffi::CStr::from_std(unsafe {
        let path: &'static str = constcat::concat!(
            lib_config::Configurator::FLEET_STABLE_CONFIGURATION_PATH,
            "\0"
        );
        std::ffi::CStr::from_bytes_with_nul_unchecked(path.as_bytes())
    })
}

#[cfg(feature = "std")]
#[no_mangle]
/// Returns a static null-terminated string with the path to the local stable config yaml config
/// file
pub extern "C" fn ddog_library_config_local_stable_config_path() -> ffi::CStr<'static> {
    // SAFETY: constcat! appends a literal "\0", guaranteeing a single null terminator
    // at the end. The path constant contains no interior null bytes.
    ffi::CStr::from_std(unsafe {
        let path: &'static str = constcat::concat!(
            lib_config::Configurator::LOCAL_STABLE_CONFIGURATION_PATH,
            "\0"
        );
        std::ffi::CStr::from_bytes_with_nul_unchecked(path.as_bytes())
    })
}

#[cfg(feature = "std")]
#[no_mangle]
pub extern "C" fn ddog_library_config_drop(mut config_result: LibraryConfigLoggedResult) {
    match &mut config_result {
        LibraryConfigLoggedResult::Ok(_) => {}
        LibraryConfigLoggedResult::Err(err) => {
            // Use the internal error clearing function for defensive cleanup
            libdd_common_ffi::clear_error(err);
        }
    }
}
