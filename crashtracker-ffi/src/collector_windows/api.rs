// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::Metadata;
use anyhow::{anyhow, Context, Result};
use core::mem::size_of;
use datadog_crashtracker::{CrashInfoBuilder, StackFrame, StackTrace, ThreadData};
use ddcommon::Endpoint;
use ddcommon_ffi::slice::AsBytes;
use ddcommon_ffi::CharSlice;
use serde::{Deserialize, Serialize};
use std::ffi::{c_void, OsString};
use std::fmt;
use std::mem::MaybeUninit;
use std::os::windows::ffi::OsStringExt;
use std::ptr::{addr_of, read_unaligned};
use std::sync::Mutex;
use windows::core::{w, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Foundation::{BOOL, ERROR_SUCCESS, E_FAIL, HANDLE, HMODULE, S_OK, TRUE};
#[cfg(target_arch = "x86_64")]
use windows::Win32::System::Diagnostics::Debug::CONTEXT_FULL_AMD64;
#[cfg(target_arch = "x86")]
use windows::Win32::System::Diagnostics::Debug::CONTEXT_FULL_X86;
use windows::Win32::System::Diagnostics::Debug::{
    AddrModeFlat, GetThreadContext, OutputDebugStringW, ReadProcessMemory, StackWalkEx,
    SymInitializeW, CONTEXT, IMAGE_DATA_DIRECTORY, IMAGE_DEBUG_DIRECTORY,
    IMAGE_DEBUG_TYPE_CODEVIEW, IMAGE_DIRECTORY_ENTRY_DEBUG, IMAGE_FILE_HEADER, IMAGE_NT_HEADERS32,
    IMAGE_NT_HEADERS64, IMAGE_OPTIONAL_HEADER_MAGIC, STACKFRAME_EX, SYM_STKWALK_DEFAULT,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::ErrorReporting::{
    WerRegisterRuntimeExceptionModule, WER_RUNTIME_EXCEPTION_INFORMATION,
};
use windows::Win32::System::ProcessStatus::{
    EnumProcessModules, GetModuleFileNameExW, GetModuleInformation, MODULEINFO,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE,
};
use windows::Win32::System::SystemInformation::{
    IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_I386,
};
use windows::Win32::System::SystemServices::{
    IMAGE_DOS_HEADER, IMAGE_DOS_SIGNATURE, IMAGE_NT_SIGNATURE,
};
use windows::Win32::System::Threading::{GetProcessId, GetThreadId, OpenThread, THREAD_ALL_ACCESS};

#[no_mangle]
#[must_use]
#[cfg(target_os = "windows")]
/// Initialize the crash-tracking infrastructure.
///
/// # Preconditions
///   None.
/// # Safety
///   Crash-tracking functions are not reentrant.
///   No other crash-handler functions should be called concurrently.
/// # Atomicity
///   This function is not atomic. A crash during its execution may lead to
///   unexpected crash-handling behaviour.
pub extern "C" fn ddog_crasht_init_windows(
    module: CharSlice,
    endpoint: Option<&Endpoint>,
    metadata: Metadata,
) -> bool {
    let result: Result<(), _> = (|| {
        let endpoint = endpoint.cloned();
        let error_context = ErrorContext {
            endpoint,
            metadata: metadata.try_into()?,
        };
        let error_context_json = serde_json::to_string(&error_context)?;
        set_error_context(&error_context_json)?;

        let path = module.try_to_string()?;
        create_registry_key(&path)?;

        unsafe {
            match WERCONTEXT.lock() {
                Ok(mut wercontext) => {
                    WerRegisterRuntimeExceptionModule(
                        &HSTRING::from(path),
                        addr_of!(*wercontext) as *const c_void,
                    )?;
                }
                Err(e) => return Err(anyhow!("Failed to lock WERCONTEXT: {}", e)),
            }
        }
        anyhow::Ok(())
    })();

    if let Err(e) = result {
        output_debug_string(format!("ddog_crasht_init_windows failed: {}", e).as_str());
        return false;
    }

    true
}

fn create_registry_key(path: &str) -> Result<()> {
    // First, check if there is already a key named "path" in SOFTWARE\Microsoft\Windows\Windows
    // Error Reporting\RuntimeExceptionHelperModules, in either HKEY_LOCAL_MACHINE or
    // HKEY_CURRENT_USER. If not, create it in HKEY_CURRENT_USER.

    let name = HSTRING::from(path);

    // Subkey path as wide string constant
    let subkey =
        w!("SOFTWARE\\Microsoft\\Windows\\Windows Error Reporting\\RuntimeExceptionHelperModules");

    // Check both HKEY_LOCAL_MACHINE and HKEY_CURRENT_USER
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let mut hkey = HKEY::default();

        // Try to open the registry key
        let open_result = unsafe { RegOpenKeyExW(root, subkey, None, KEY_READ, &mut hkey) };

        if open_result == ERROR_SUCCESS {
            // Check if the value exists
            let query_result = unsafe { RegQueryValueExW(hkey, &name, None, None, None, None) };

            let _ = unsafe { RegCloseKey(hkey) };

            if query_result == ERROR_SUCCESS {
                // Value exists in either hive, exit successfully
                return Ok(());
            }
        }
    }

    // Value doesn't exist in either hive, create in HKEY_CURRENT_USER
    let mut hkey = HKEY::default();

    // Create or open the key with write access
    let create_result = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };

    anyhow::ensure!(
        create_result == ERROR_SUCCESS,
        "Failed to create registry key {path}"
    );

    // Create the DWORD value (0)
    let dword_value: u32 = 0;
    let dword_bytes = dword_value.to_ne_bytes();
    let set_value_result =
        unsafe { RegSetValueExW(hkey, &name, None, REG_DWORD, Some(&dword_bytes)) };

    anyhow::ensure!(
        set_value_result == ERROR_SUCCESS,
        "Failed to set registry value"
    );

    let _ = unsafe { RegCloseKey(hkey) };

    Ok(())
}

fn output_debug_string(message: &str) {
    unsafe { OutputDebugStringW(&HSTRING::from(message)) };
}

fn set_error_context(message: &str) -> Result<()> {
    let bytes = message.as_bytes();
    let boxed_slice = bytes.to_vec().into_boxed_slice();
    let static_slice = Box::leak(boxed_slice);

    match WERCONTEXT.lock() {
        Ok(mut wercontext) => {
            wercontext.ptr = static_slice.as_ptr() as usize;
            wercontext.len = static_slice.len();
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!("Failed to lock WERCONTEXT: {}", e)),
    }
}

#[derive(Serialize, Deserialize)]
pub struct ErrorContext {
    endpoint: Option<Endpoint>,
    metadata: datadog_crashtracker::Metadata,
}

static WERCONTEXT: Mutex<WerContext> = Mutex::new(WerContext {
    prefix: WER_CONTEXT_PREFIX,
    ptr: 0,
    len: 0,
    suffix: WER_CONTEXT_SUFFIX,
});

// There is no meaning to those patterns, they are just used to validate the WerContext
// If needed, they can be repurposed for versioning
pub const WER_CONTEXT_PREFIX: u64 = 0xBABA_BABA_BABA_BABA;
pub const WER_CONTEXT_SUFFIX: u64 = 0xEFEF_EFEF_EFEF_EFEF;

#[repr(C)]
pub struct WerContext {
    prefix: u64,
    ptr: usize,
    len: usize,
    suffix: u64,
}

#[no_mangle]
#[cfg(target_os = "windows")]
pub extern "C" fn OutOfProcessExceptionEventSignatureCallback(
    _context: *const c_void,
    _exception_information: *const WER_RUNTIME_EXCEPTION_INFORMATION,
    _index: i32,
    _name: *mut u16,
    _name_size: *mut u32,
    _value: *mut u16,
    _value_size: *mut u32,
) -> HRESULT {
    // This callback is not supposed to be called by WER because we don't claim the crash,
    // but we need to define it anyway because WER checks for its presence.
    output_debug_string("OutOfProcessExceptionEventSignatureCallback");
    S_OK
}

#[no_mangle]
#[cfg(target_os = "windows")]
pub extern "C" fn OutOfProcessExceptionEventDebuggerLaunchCallback(
    _context: *const c_void,
    _exception_information: *const WER_RUNTIME_EXCEPTION_INFORMATION,
    _is_custom_debugger: *mut BOOL,
    _debugger_launch: *mut u16,
    _debugger_launch_size: *mut u32,
    _is_debugger_auto_launch: *mut BOOL,
) -> HRESULT {
    // This callback is not supposed to be called by WER because we don't claim the crash,
    // but we need to define it anyway because WER checks for its presence.
    output_debug_string("OutOfProcessExceptionEventDebuggerLaunchCallback");
    S_OK
}

#[no_mangle]
#[cfg(target_os = "windows")]
pub extern "C" fn OutOfProcessExceptionEventCallback(
    context: *const c_void,
    exception_information: *const WER_RUNTIME_EXCEPTION_INFORMATION,
    _ownership_claimed: *mut BOOL,
    _event_name: *mut u16,
    _size: *mut u32,
    _signature_count: *mut u32,
) -> HRESULT {
    output_debug_string("OutOfProcessExceptionEventCallback");

    let result: Result<(), _> = (|| {
        anyhow::ensure!(
            !exception_information.is_null(),
            "exception_information is null"
        );

        let exception_information = unsafe { *exception_information };
        unsafe { SymInitializeW(exception_information.hProcess, PCWSTR::null(), true)? };

        let pid = unsafe { GetProcessId(exception_information.hProcess) };
        let crash_tid = unsafe { GetThreadId(exception_information.hThread) };
        let threads = list_threads(pid).context("Failed to list threads")?;
        let modules = list_modules(exception_information.hProcess).unwrap_or_default();

        let wer_context = read_wer_context(exception_information.hProcess, context as usize)?;

        let error_context_json =
            unsafe { std::slice::from_raw_parts(wer_context.ptr as *const u8, wer_context.len) };
        let error_context_str = std::str::from_utf8(error_context_json)?;
        let error_context: ErrorContext = serde_json::from_str(error_context_str)?;

        let mut builder = CrashInfoBuilder::new();

        for thread in threads {
            let stack_result = walk_thread_stack(exception_information.hProcess, thread, &modules);

            let stack: StackTrace = stack_result.unwrap_or_else(|e| {
                output_debug_string(format!("Failed to walk thread stack: {}", e).as_str());
                StackTrace::new_incomplete()
            });

            if thread == crash_tid {
                builder
                    .with_stack(stack.clone())
                    .context("Failed to add crashed thread info")?;
            }

            let thread_data = ThreadData {
                crashed: thread == crash_tid,
                name: thread.to_string(),
                stack,
                state: None,
            };

            builder
                .with_thread(thread_data)
                .context("Failed to add thread info")?;
        }
        builder
            .with_kind(datadog_crashtracker::ErrorKind::Panic)
            .context("Failed to add error kind")?;
        builder
            .with_os_info_this_machine()
            .context("Failed to add OS info")?;
        builder
            .with_incomplete(false)
            .context("Failed to set incomplete to false")?;
        builder
            .with_metadata(error_context.metadata)
            .context("Failed to add metadata")?;
        let crash_info = builder.build().context("Failed to build crash info")?;

        crash_info
            .upload_to_endpoint(&error_context.endpoint)
            .context("Failed to upload crash info")?;

        anyhow::Ok(())
    })();

    if let Err(e) = result {
        output_debug_string(format!("OutOfProcessExceptionEventCallback failed: {}", e).as_str());
        return E_FAIL;
    }

    output_debug_string("OutOfProcessExceptionEventCallback succeeded");
    S_OK
}

fn read_wer_context(process_handle: HANDLE, base_address: usize) -> Result<WerContext> {
    let buffer = read_memory_raw(process_handle, base_address as u64, size_of::<WerContext>())?;

    anyhow::ensure!(
        buffer.len() == size_of::<WerContext>(),
        "Failed to read the full WerContext, wrong size"
    );

    // Create a MaybeUninit to hold the WerContext
    let mut wer_context = MaybeUninit::<WerContext>::uninit();

    // Copy the raw bytes from the Vec<u8> into the MaybeUninit<WerContext>
    let ptr = wer_context.as_mut_ptr();
    unsafe { std::ptr::copy_nonoverlapping(buffer.as_ptr(), ptr as *mut u8, buffer.len()) };

    // Validate prefix and suffix
    let raw_ptr = wer_context.as_ptr();

    // We can't call assume_init yet because ptr hasn't been set,
    // and apparently (*raw_ptr).prefix before calling assume_init is undefined behavior
    let prefix = unsafe { read_unaligned(addr_of!((*raw_ptr).prefix)) };
    let suffix = unsafe { read_unaligned(addr_of!((*raw_ptr).suffix)) };

    anyhow::ensure!(
        prefix == WER_CONTEXT_PREFIX && suffix == WER_CONTEXT_SUFFIX,
        "Invalid WER context"
    );

    let ptr = unsafe { read_unaligned(addr_of!((*raw_ptr).ptr)) };
    let len = unsafe { read_unaligned(addr_of!((*raw_ptr).len)) };

    anyhow::ensure!(ptr != 0 && len > 0, "Invalid WER context");

    // Read the memory in the target process pointed by ptr
    let buffer = read_memory_raw(process_handle, ptr as u64, len)?;

    // Copy the buffer into a new Box<[u8]>
    let boxed_slice = buffer.into_boxed_slice();

    // Leak the Box<[u8]> to get a static reference
    let static_slice = Box::leak(boxed_slice);

    // Create a new WerContext with the leaked static reference
    let wer_context = WerContext {
        prefix: WER_CONTEXT_PREFIX,
        ptr: static_slice.as_ptr() as usize,
        len,
        suffix: WER_CONTEXT_SUFFIX,
    };

    Ok(wer_context)
}

// https://github.com/microsoft/win32metadata/issues/1044
#[repr(align(16))]
#[derive(Default)]
struct AlignedContext {
    ctx: CONTEXT,
}

fn walk_thread_stack(
    process_handle: HANDLE,
    thread_id: u32,
    modules: &[ModuleInfo],
) -> Result<StackTrace> {
    let mut stacktrace = StackTrace::new_incomplete();
    let thread_handle = unsafe { OpenThread(THREAD_ALL_ACCESS, false, thread_id)? };
    let mut context = AlignedContext::default();

    #[cfg(target_arch = "x86_64")]
    {
        context.ctx.ContextFlags = CONTEXT_FULL_AMD64;
    }
    #[cfg(target_arch = "x86")]
    {
        context.ctx.ContextFlags = CONTEXT_FULL_X86;
    }

    unsafe { GetThreadContext(thread_handle, &mut context.ctx)? };

    let mut native_frame = STACKFRAME_EX::default();
    let machine_type: u32;

    #[cfg(target_arch = "x86_64")]
    {
        machine_type = IMAGE_FILE_MACHINE_AMD64.0 as u32;
        native_frame.AddrPC.Offset = context.ctx.Rip;
        native_frame.AddrPC.Mode = AddrModeFlat;
        native_frame.AddrStack.Offset = context.ctx.Rsp;
        native_frame.AddrStack.Mode = AddrModeFlat;
        native_frame.AddrFrame.Offset = context.ctx.Rbp;
        native_frame.AddrFrame.Mode = AddrModeFlat;
    }

    #[cfg(target_arch = "x86")]
    {
        machine_type = IMAGE_FILE_MACHINE_I386.0 as u32;
        native_frame.AddrPC.Offset = context.ctx.Eip as u64;
        native_frame.AddrPC.Mode = AddrModeFlat;
        native_frame.AddrStack.Offset = context.ctx.Esp as u64;
        native_frame.AddrStack.Mode = AddrModeFlat;
        native_frame.AddrFrame.Offset = context.ctx.Ebp as u64;
        native_frame.AddrFrame.Mode = AddrModeFlat;
    }

    while let TRUE = unsafe {
        StackWalkEx(
            machine_type,
            process_handle,
            thread_handle,
            &mut native_frame,
            &mut context as *mut _ as *mut c_void,
            None,
            None,
            None,
            None,
            SYM_STKWALK_DEFAULT,
        )
    } {
        let mut frame = StackFrame::new();

        frame.ip = Some(format!("{:x}", native_frame.AddrPC.Offset));
        frame.sp = Some(format!("{:x}", native_frame.AddrStack.Offset));

        // Find the module
        let module = modules.iter().find(|module| {
            module.base_address <= native_frame.AddrPC.Offset
                && native_frame.AddrPC.Offset < module.base_address + module.size
        });

        if let Some(module) = module {
            frame.module_base_address = Some(format!("{:x}", module.base_address));
            frame.symbol_address = Some(format!(
                "{:x}",
                native_frame.AddrPC.Offset - module.base_address
            ));
            frame.path.clone_from(&module.path);

            if let Some(pdb_info) = &module.pdb_info {
                frame.build_id = Some(format!("{:x}{:x}", pdb_info.signature, pdb_info.age));
                frame.build_id_type = Some(datadog_crashtracker::BuildIdType::PDB);
                frame.file_type = Some(datadog_crashtracker::FileType::PE);
            }
        }

        stacktrace
            .push_frame(frame, true)
            .context("Failed to add frame")?;
    }

    stacktrace.set_complete()?;
    Ok(stacktrace)
}

struct ModuleInfo {
    base_address: u64,
    size: u64,
    path: Option<String>,
    pdb_info: Option<PdbInfo>,
}

struct PdbInfo {
    age: u32,
    signature: Guid,
}

fn list_modules(process_handle: HANDLE) -> anyhow::Result<Vec<ModuleInfo>> {
    // Use EnumProcessModules to get a list of modules
    let mut module_infos = Vec::new();

    // Get the number of bytes required to store the array of module handles
    let mut cb_needed = 0;
    unsafe { EnumProcessModules(process_handle, std::ptr::null_mut(), 0, &mut cb_needed) }
        .context("Failed to get module list size")?;

    // Allocate enough space for the module handles
    let modules_count = cb_needed as usize / size_of::<HMODULE>();
    let mut hmodules: Vec<HMODULE> = Vec::with_capacity(modules_count);
    let mut cb_actual = 0;

    unsafe {
        EnumProcessModules(
            process_handle,
            hmodules.as_mut_ptr(),
            cb_needed,
            &mut cb_actual,
        )
    }
    .context("Failed to enumerate process modules")?;

    unsafe { hmodules.set_len(cb_actual as usize / size_of::<HMODULE>()) };

    // Iterate through the module handles and retrieve information
    for &hmodule in hmodules.iter() {
        let mut module_info = MODULEINFO::default();
        if unsafe {
            GetModuleInformation(
                process_handle,
                hmodule,
                &mut module_info,
                size_of::<MODULEINFO>() as u32,
            )
        }
        .is_err()
        {
            continue;
        }

        let module_path = get_module_path(process_handle, hmodule);

        module_infos.push(ModuleInfo {
            base_address: module_info.lpBaseOfDll as u64,
            size: module_info.SizeOfImage as u64,
            path: module_path.ok(),
            pdb_info: get_pdb_info(process_handle, module_info.lpBaseOfDll as u64).ok(),
        });
    }

    Ok(module_infos)
}

/// Read a value from the target process memory.
/// # Safety
/// This function reads memory from another process and casts it to an arbitrary type.
/// Make sure to perform proper validation on the data before using it, and don't use it for
/// types that have references, because the references are relative to the target process.
unsafe fn read_memory<T>(process_handle: HANDLE, address: u64) -> Result<T> {
    let mut bytes_read = 0;
    let mut value = MaybeUninit::<T>::uninit();
    let size = size_of::<T>();

    let result = unsafe {
        ReadProcessMemory(
            process_handle,
            address as *const _,
            value.as_mut_ptr() as *mut _,
            size,
            Some(&mut bytes_read),
        )
    };

    anyhow::ensure!(
        result.is_ok() && bytes_read == size,
        "Failed to read memory"
    );

    Ok(value.assume_init())
}

fn read_memory_raw(process_handle: HANDLE, address: u64, size: usize) -> Result<Vec<u8>> {
    let mut buffer = vec![0u8; size];
    let mut bytes_read = 0;

    let result = unsafe {
        ReadProcessMemory(
            process_handle,
            address as *const _,
            buffer.as_mut_ptr() as *mut _,
            size,
            Some(&mut bytes_read),
        )
    };

    if result.is_err() || bytes_read != size {
        return Err(anyhow!("Failed to read memory"));
    }

    Ok(buffer)
}

#[repr(C)]
struct ImageNtHeadersGeneric {
    pub signature: u32,
    pub file_header: IMAGE_FILE_HEADER,
    pub magic: IMAGE_OPTIONAL_HEADER_MAGIC,
}

#[repr(C)]
#[derive(Debug)]
struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl fmt::LowerHex for Guid {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{:08x}{:04x}{:04x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.data1,
            self.data2,
            self.data3,
            self.data4[0],
            self.data4[1],
            self.data4[2],
            self.data4[3],
            self.data4[4],
            self.data4[5],
            self.data4[6],
            self.data4[7]
        )
    }
}

#[repr(C)]
struct CvInfoPdb70 {
    pub signature: u32,
    pub guid: Guid,
    pub age: u32,
}

fn get_pdb_info(process_handle: HANDLE, base_address: u64) -> Result<PdbInfo> {
    let dos_header: IMAGE_DOS_HEADER = unsafe { read_memory(process_handle, base_address)? };

    if dos_header.e_magic != IMAGE_DOS_SIGNATURE {
        return Err(anyhow!("Invalid DOS header"));
    }

    let nt_headers_address = base_address + dos_header.e_lfanew as u64;
    let nt_headers: ImageNtHeadersGeneric =
        unsafe { read_memory(process_handle, nt_headers_address)? };

    if nt_headers.signature != IMAGE_NT_SIGNATURE {
        return Err(anyhow!("Invalid NT headers"));
    }

    let is_pe64 = nt_headers.file_header.Machine == IMAGE_FILE_MACHINE_AMD64;
    let is_pe32 = nt_headers.file_header.Machine == IMAGE_FILE_MACHINE_I386;

    if !is_pe32 && !is_pe64 {
        return Err(anyhow!("Invalid machine type"));
    }

    let debug_data_dir: IMAGE_DATA_DIRECTORY = if is_pe32 {
        let nt_headers32: IMAGE_NT_HEADERS32 =
            unsafe { read_memory(process_handle, nt_headers_address)? };
        nt_headers32.OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_DEBUG.0 as usize]
    } else {
        let nt_headers64: IMAGE_NT_HEADERS64 =
            unsafe { read_memory(process_handle, nt_headers_address)? };
        nt_headers64.OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_DEBUG.0 as usize]
    };

    let debug_dir_buffer = read_memory_raw(
        process_handle,
        base_address + debug_data_dir.VirtualAddress as u64,
        debug_data_dir.Size as usize,
    )?;

    let debug_dir = unsafe {
        std::slice::from_raw_parts(
            debug_dir_buffer.as_ptr() as *const IMAGE_DEBUG_DIRECTORY,
            debug_dir_buffer.len() / std::mem::size_of::<IMAGE_DEBUG_DIRECTORY>(),
        )
    };

    for entry in debug_dir {
        if entry.Type != IMAGE_DEBUG_TYPE_CODEVIEW {
            continue;
        }

        let cv_info: CvInfoPdb70 =
            unsafe { read_memory(process_handle, base_address + entry.AddressOfRawData as u64)? };

        if cv_info.signature == 0x53445352
        /* 'RSDS' */
        {
            return Ok(PdbInfo {
                age: cv_info.age,
                signature: cv_info.guid,
            });
        }
    }

    anyhow::bail!("No CodeView entry found");
}

fn get_module_path(process_handle: HANDLE, module_handle: HMODULE) -> Result<String> {
    let mut module_name_buffer = vec![0u16; 1024];

    let len = unsafe {
        GetModuleFileNameExW(
            Some(process_handle),
            Some(module_handle),
            &mut module_name_buffer,
        )
    };

    if len == 0 {
        return Err(anyhow!("GetModuleFileNameExW failed: {}", len));
    }

    let module_name = OsString::from_wide(&module_name_buffer[..len as usize])
        .to_string_lossy()
        .into_owned();

    Ok(module_name)
}

fn list_threads(pid: u32) -> Result<Vec<u32>> {
    let mut thread_ids = Vec::new();

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, pid)? };

    let mut thread_entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };

    if unsafe { Thread32First(snapshot, &mut thread_entry) }.is_ok() {
        loop {
            if thread_entry.th32OwnerProcessID == pid {
                thread_ids.push(thread_entry.th32ThreadID);
            }

            if unsafe { Thread32Next(snapshot, &mut thread_entry) }.is_err() {
                break;
            }
        }
    }

    Ok(thread_ids)
}

#[cfg(test)]
mod tests {
    use crate::collector_windows::api::*;
    use goblin::pe::PE;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::ProcessStatus::{GetModuleInformation, MODULEINFO};
    use windows::Win32::System::Threading::GetCurrentProcess;

    #[test]
    fn test_wercontext() {
        let expected_data = vec![0x01, 0x02, 0x03, 0x04];
        let expected_size = expected_data.len() * size_of::<u8>();
        let expected_context = WerContext {
            prefix: WER_CONTEXT_PREFIX,
            len: expected_size,
            ptr: expected_data.as_ptr() as usize,
            suffix: WER_CONTEXT_SUFFIX,
        };

        let process_handle = unsafe { GetCurrentProcess() };
        let result = read_wer_context(process_handle, &expected_context as *const _ as usize);
        let actual_context = result.unwrap();

        assert_eq!(actual_context.prefix, WER_CONTEXT_PREFIX);
        assert_eq!(actual_context.len, expected_size);
        assert_eq!(actual_context.suffix, WER_CONTEXT_SUFFIX);
        // read_wer_context makes a copy, so the address should be different
        assert_ne!(actual_context.ptr, expected_context.ptr);

        let buffer = unsafe {
            std::slice::from_raw_parts(actual_context.ptr as *const u8, actual_context.len)
        };
        assert_eq!(buffer, expected_data);
    }

    #[test]
    fn test_invalid_wercontext() {
        let data = [0x01, 0x02, 0x03, 0x04];
        let mut context = WerContext {
            prefix: 0,
            len: data.len() * size_of::<u8>(),
            ptr: data.as_ptr() as usize,
            suffix: 0,
        };

        let process_handle = unsafe { GetCurrentProcess() };

        // Valid prefix, invalid suffix
        context.prefix = WER_CONTEXT_PREFIX;
        context.suffix = 0;
        let result = read_wer_context(process_handle, &context as *const _ as usize);
        assert!(result.is_err());

        // Invalid prefix, valid suffix
        context.suffix = WER_CONTEXT_SUFFIX;
        context.prefix = 0;
        let result = read_wer_context(process_handle, &context as *const _ as usize);
        assert!(result.is_err());

        // Valid prefix, valid suffix
        context.suffix = WER_CONTEXT_SUFFIX;
        context.prefix = WER_CONTEXT_PREFIX;
        let result = read_wer_context(process_handle, &context as *const _ as usize);
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_pdb_info() {
        let process_handle = unsafe { GetCurrentProcess() };
        let module_handle = unsafe { GetModuleHandleW(None).unwrap() };
        let mut module_info = MODULEINFO::default();
        unsafe {
            GetModuleInformation(
                process_handle,
                module_handle,
                &mut module_info,
                size_of::<MODULEINFO>() as u32,
            )
        }
        .unwrap();
        let base_address = module_info.lpBaseOfDll as u64;
        let path = get_module_path(process_handle, module_handle).unwrap();

        let file = std::fs::read(path).unwrap();
        let pe = PE::parse(&file).unwrap();

        let expected_debug_id = pe.debug_data.unwrap().codeview_pdb70_debug_info.unwrap();
        let expected_pdb_signature = expected_debug_id.signature;
        let expected_pdb_age = expected_debug_id.age;

        let actual_pdb_info = get_pdb_info(process_handle, base_address).unwrap();

        assert_eq!(actual_pdb_info.age, expected_pdb_age);

        let bytes = unsafe {
            std::slice::from_raw_parts(
                &actual_pdb_info.signature as *const Guid as *const u8,
                size_of::<Guid>(),
            )
        };
        assert_eq!(bytes, expected_pdb_signature);
    }
}
