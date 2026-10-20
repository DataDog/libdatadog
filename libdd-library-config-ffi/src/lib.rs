// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
// #![no_std] is only set under `standalone`: a staticlib/cdylib owns the
// allocator and panic handler, while a `lib` consumer provides its own.
#![cfg_attr(all(not(feature = "std"), feature = "standalone"), no_std)]

extern crate alloc;

#[cfg(all(not(feature = "std"), feature = "standalone", not(panic = "abort")))]
compile_error!(
    "The `standalone` feature requires `panic = \"abort\"` in the Cargo profile. \
     Building with panic=unwind causes undefined behavior at FFI boundaries."
);

#[cfg(all(not(feature = "std"), feature = "standalone"))]
mod no_std_support {
    // Forward to the host's libc allocator; this library is always loaded into a C process.
    #[global_allocator]
    static ALLOC: libc_alloc::LibcAlloc = libc_alloc::LibcAlloc;

    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo) -> ! {
        // Panics in no_std mode are silent and fatal — no std I/O is available to report _info.
        // Declared inside the function so cbindgen does not emit `abort` into the C header.
        // no_std links with -nodefaultlibs, so libc must be requested explicitly; this resolves
        // `abort` as well as the malloc/free family used by libc_alloc.
        #[cfg_attr(target_vendor = "apple", link(name = "System"))]
        #[cfg_attr(target_os = "linux", link(name = "c"))]
        unsafe extern "C" {
            fn abort() -> !;
        }
        // SAFETY: abort() is a C standard library function with no preconditions.
        unsafe { abort() }
    }

    // `rust_eh_personality` is referenced by the precompiled core/alloc (built with
    // panic=unwind), so it must be defined for the link to succeed. A no-op is safe because
    // unwinding never occurs under `panic = "abort"` (enforced by the compile_error! guard
    // above). It is defined in asm so it can be hidden and weak: hidden keeps it out of the
    // cdylib's dynamic symbol table, so it cannot interpose on another Rust library's
    // personality at runtime; weak lets a real personality from an unwinding Rust staticlib
    // win when both are linked into the same binary.
    #[cfg(target_vendor = "apple")]
    core::arch::global_asm!(
        ".globl _rust_eh_personality",
        ".weak_definition _rust_eh_personality",
        ".private_extern _rust_eh_personality",
        "_rust_eh_personality:",
        "ret",
    );
    #[cfg(target_os = "linux")]
    core::arch::global_asm!(
        ".globl rust_eh_personality",
        ".weak rust_eh_personality",
        ".hidden rust_eh_personality",
        ".type rust_eh_personality, %function",
        "rust_eh_personality:",
        "ret",
    );
    // No hidden/weak equivalent in COFF asm; fall back to a plain exported definition.
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    #[unsafe(no_mangle)]
    pub extern "C" fn rust_eh_personality() {}
}

pub mod tracer_metadata;

use libdd_common_ffi::{self as ffi, slice::AsBytes};

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;

use ffi::CharSlice;
use libdd_library_config::{self as lib_config, LibraryConfigSource};

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

pub struct Configurator<'a> {
    #[cfg_attr(
        not(feature = "std"),
        expect(dead_code, reason = "only read by the std-only getters")
    )]
    inner: lib_config::Configurator,
    #[cfg_attr(
        not(feature = "std"),
        expect(dead_code, reason = "only read by the std-only getters")
    )]
    language: CharSlice<'a>,
    fleet_path: Option<ffi::CStr<'a>>,
    local_path: Option<ffi::CStr<'a>>,
    process_info: Option<lib_config::ProcessInfo>,
}

#[unsafe(no_mangle)]
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

#[unsafe(no_mangle)]
pub extern "C" fn ddog_library_configurator_with_local_path<'a>(
    c: &mut Configurator<'a>,
    local_path: ffi::CStr<'a>,
) {
    c.local_path = Some(local_path);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_library_configurator_with_fleet_path<'a>(
    c: &mut Configurator<'a>,
    local_path: ffi::CStr<'a>,
) {
    c.fleet_path = Some(local_path);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_library_configurator_with_process_info<'a>(
    c: &mut Configurator<'a>,
    p: ProcessInfo<'a>,
) {
    c.process_info = Some(p.ffi_to_rs());
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_library_configurator_drop(_: Box<Configurator>) {}

#[cfg(feature = "std")]
mod std_api {
    use super::*;
    use libdd_common_ffi::{CString, Error};

    #[cfg(all(feature = "catch_panic", panic = "unwind"))]
    use core::panic::AssertUnwindSafe;
    #[cfg(all(feature = "catch_panic", panic = "unwind"))]
    use std::panic::catch_unwind;

    #[cfg(all(feature = "catch_panic", panic = "unwind"))]
    macro_rules! catch_panic {
        ($f:expr) => {
            match catch_unwind(AssertUnwindSafe(|| $f)) {
                Ok(ret) => ret,
                Err(info) => {
                    let panic_msg = if let Some(s) = info.downcast_ref::<&'static str>() {
                        s.to_string()
                    } else if let Some(s) = info.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "Unable to retrieve panic context".to_string()
                    };
                    LibraryConfigLoggedResult::Err(Error::from(format!(
                        "FFI function panicked: {}",
                        panic_msg
                    )))
                }
            }
        };
    }

    #[cfg(any(not(feature = "catch_panic"), panic = "abort"))]
    macro_rules! catch_panic {
        ($f:expr) => {
            $f
        };
    }

    /// A result type that includes debug/log messages along with the data
    #[repr(C)]
    pub struct OkResult {
        pub value: ffi::Vec<LibraryConfig>,
        pub logs: CString,
    }

    #[repr(C)]
    pub enum LibraryConfigLoggedResult {
        Ok(OkResult),
        Err(Error),
    }

    impl LibraryConfig {
        fn rs_vec_to_ffi(
            configs: Vec<lib_config::LibraryConfig>,
        ) -> anyhow::Result<ffi::Vec<Self>> {
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

        fn logged_result_to_ffi_with_messages(
            result: libdd_library_config::LoggedResult<
                Vec<lib_config::LibraryConfig>,
                anyhow::Error,
            >,
        ) -> LibraryConfigLoggedResult {
            match result {
                libdd_library_config::LoggedResult::Ok(configs, logs) => {
                    match Self::rs_vec_to_ffi(configs) {
                        Ok(ffi_configs) => {
                            let messages = logs.join("\n");
                            let cstring_logs = CString::new_or_empty(messages);
                            LibraryConfigLoggedResult::Ok(OkResult {
                                value: ffi_configs,
                                logs: cstring_logs,
                            })
                        }
                        Err(err) => LibraryConfigLoggedResult::Err(err.into()),
                    }
                }
                libdd_library_config::LoggedResult::Err(err) => {
                    LibraryConfigLoggedResult::Err(err.into())
                }
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn ddog_library_configurator_with_detect_process_info(c: &mut Configurator) {
        c.process_info = Some(lib_config::ProcessInfo::detect_global(
            c.language.to_utf8_lossy().into_owned(),
        ));
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn ddog_library_configurator_get(
        configurator: &Configurator,
    ) -> LibraryConfigLoggedResult {
        catch_panic!({
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
        })
    }

    #[unsafe(no_mangle)]
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

    /// Wraps a `&'static str` path constant as a `ffi::CStr<'static>` by appending a
    /// compile-time NUL terminator.
    ///
    /// SAFETY: `constcat::concat!` appends a literal "\0", guaranteeing a single trailing
    /// null. The caller is responsible for ensuring the path constant has no interior nulls.
    macro_rules! static_path_cstr {
        ($path:expr) => {
            ffi::CStr::from_std(unsafe {
                let path: &'static str = constcat::concat!($path, "\0");
                core::ffi::CStr::from_bytes_with_nul_unchecked(path.as_bytes())
            })
        };
    }

    #[unsafe(no_mangle)]
    /// Returns a static null-terminated string with the path to the managed stable config yaml
    /// config file
    pub extern "C" fn ddog_library_config_fleet_stable_config_path() -> ffi::CStr<'static> {
        static_path_cstr!(lib_config::Configurator::FLEET_STABLE_CONFIGURATION_PATH)
    }

    #[unsafe(no_mangle)]
    /// Returns a static null-terminated string with the path to the local stable config yaml config
    /// file
    pub extern "C" fn ddog_library_config_local_stable_config_path() -> ffi::CStr<'static> {
        static_path_cstr!(lib_config::Configurator::LOCAL_STABLE_CONFIGURATION_PATH)
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn ddog_library_config_drop(mut config_result: LibraryConfigLoggedResult) {
        match &mut config_result {
            LibraryConfigLoggedResult::Ok(_) => {}
            LibraryConfigLoggedResult::Err(err) => {
                libdd_common_ffi::clear_error(err);
            }
        }
    }
}

#[cfg(feature = "std")]
pub use std_api::*;
