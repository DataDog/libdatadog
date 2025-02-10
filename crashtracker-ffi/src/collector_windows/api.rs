// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{anyhow, Result};
use datadog_crashtracker::{CrashInfoBuilder, StackFrame, StackTrace, ThreadData};
use ddcommon::Endpoint;
use function_name::named;
use std::ffi::{c_void, OsString};
use std::fmt;
use std::mem::MaybeUninit;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::ptr::{addr_of, read_unaligned};
use windows::core::{s, HRESULT, PCWSTR};
use windows::Win32::Foundation::{GetLastError, BOOL, HANDLE, HMODULE, MAX_PATH, S_OK, TRUE};
use windows::Win32::System::Diagnostics::Debug::{AddrModeFlat, GetThreadContext, OutputDebugStringA, ReadProcessMemory, StackWalkEx, SymInitializeW, CONTEXT, CONTEXT_FULL_AMD64, IMAGE_DATA_DIRECTORY, IMAGE_DEBUG_DIRECTORY, IMAGE_DEBUG_TYPE_CODEVIEW, IMAGE_DIRECTORY_ENTRY_DEBUG, IMAGE_FILE_HEADER, IMAGE_NT_HEADERS32, IMAGE_NT_HEADERS64, IMAGE_OPTIONAL_HEADER_MAGIC, STACKFRAME_EX, SYM_STKWALK_DEFAULT};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32};
use windows::Win32::System::ErrorReporting::{WerRegisterRuntimeExceptionModule, WER_RUNTIME_EXCEPTION_INFORMATION};
use windows::Win32::System::ProcessStatus::{EnumProcessModules, GetModuleFileNameExW, GetModuleInformation, MODULEINFO};
use windows::Win32::System::SystemInformation::{IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_I386};
use windows::Win32::System::SystemServices::{IMAGE_DOS_HEADER, IMAGE_DOS_SIGNATURE, IMAGE_NT_SIGNATURE};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessId, GetThreadId, OpenThread, THREAD_ALL_ACCESS};
use ddcommon_ffi::{wrap_with_void_ffi_result, CharSlice, Slice, VoidResult};
use serde::{Deserialize, Serialize};
use log::error;
use windows::core::imp::HSTRING;
use ddcommon_ffi::slice::AsBytes;
use crate::Metadata;

#[no_mangle]
#[must_use]
#[named]
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
pub unsafe extern "C" fn ddog_crasht_init_windows(
    module: HMODULE,
    endpoint: Option<&mut Endpoint>,
    metadata: Metadata,
) -> bool {
    let result: Result<(), _> = (|| {
        let endpoint_option = if endpoint.is_none() {
            None
        } else {
            Some(endpoint.unwrap().clone())
        };

        let error_context = ErrorContext { endpoint: endpoint_option, metadata: metadata.try_into()? };
        let error_context_json = serde_json::to_string(&error_context).unwrap();
        set_error_context(&error_context_json);

        let path = get_module_path(GetCurrentProcess(), module)?;
        let wpath: Vec<u16> = path.encode_utf16().collect();

        WerRegisterRuntimeExceptionModule(PCWSTR::from_raw(wpath.as_ptr()), &WERCONTEXT as *const WerContext as *const c_void)?;

        Ok::<(), anyhow::Error>(())
    })();

    if result.is_err()
    {
        return false;
    }

    true
}

#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_get_wercontext_for_tests() -> *const c_void {
    return &WERCONTEXT as *const WerContext as *const c_void;
}

fn set_error_context(message: &str) {
    let bytes = message.as_bytes();
    let boxed_slice = bytes.to_vec().into_boxed_slice();
    let static_slice = Box::leak(boxed_slice);

    unsafe {
        WERCONTEXT.ptr = static_slice.as_ptr();
        WERCONTEXT.len = static_slice.len();
    }
}

#[derive(Serialize, Deserialize)]
pub struct ErrorContext {
    endpoint: Option<Endpoint>,
    metadata: datadog_crashtracker::Metadata
}

static mut WERCONTEXT: WerContext = WerContext {
    prefix: WER_CONTEXT_PREFIX,
    ptr: std::ptr::null(),
    len: 0,
    suffix: WER_CONTEXT_SUFFIX
};

const WER_CONTEXT_PREFIX: u64 = 0xBABA_BABA_BABA_BABA;
const WER_CONTEXT_SUFFIX: u64 = 0xEFEF_EFEF_EFEF_EFEF;

#[repr(C)]
pub struct WerContext
{
    prefix: u64,
    ptr: *const u8,
    len: usize,
    suffix: u64
}

#[no_mangle]
#[named]
#[cfg(windows)]
pub unsafe extern "C" fn exception_event_callback(
    pContext: *const c_void,
    pExceptionInformation: *const WER_RUNTIME_EXCEPTION_INFORMATION,
    pbOwnershipClaimed: *mut BOOL,
    pwszEventName: *mut u16,
    pchSize: *mut u32,
    pdwSignatureCount: *mut u32,
) -> HRESULT {
    let exception_information = *pExceptionInformation;
    let sym_init_result = SymInitializeW(exception_information.hProcess, PCWSTR::null(), true);

    if sym_init_result.is_err() {
        return HRESULT::from_win32(sym_init_result.err().unwrap().code().0 as u32);
    }

    let pid = GetProcessId(exception_information.hProcess);
    let crash_tid = GetThreadId(exception_information.hThread);
    let threads = list_threads(pid);
    let modules = list_modules(exception_information.hProcess).unwrap_or_else(|_| Vec::new());

    let wer_context = read_wer_context(exception_information.hProcess, pContext as usize).unwrap();

    let error_context_json = std::slice::from_raw_parts(wer_context.ptr, wer_context.len);
    let error_context_str = std::str::from_utf8(error_context_json).unwrap();
    let error_context: ErrorContext = serde_json::from_str(error_context_str).unwrap();

    let mut builder = CrashInfoBuilder::new();

    for thread in threads.unwrap() {
        let stack = walk_thread_stack(exception_information.hProcess, thread, &modules).unwrap_or_else(|_| StackTrace::new_incomplete());

        if (thread == crash_tid) {
            builder.with_stack(stack.clone()).expect("Failed to add crashed thread info");
        }

        let thread_data = ThreadData {
            crashed: thread == crash_tid,
            name: format!("{}", thread),
            stack: stack,
            state: None,
        };

        builder.with_thread(thread_data).expect("Failed to add thread info");
    }
    builder.with_kind(datadog_crashtracker::ErrorKind::Panic).expect("Failed to add error kind");
    builder.with_os_info_this_machine().expect("Failed to add OS info");
    builder.with_incomplete(false).expect("Failed to set incomplete to false");
    builder.with_metadata(error_context.metadata).expect("Failed to add metadata");
    let crash_info = builder.build().expect("Failed to build crash info");

    crash_info
        .upload_to_endpoint(&error_context.endpoint)
        .expect("Failed to upload crash info");

    return S_OK;
}

pub unsafe fn read_wer_context(process_handle: HANDLE, base_address: usize) -> Result<WerContext> {
    let buffer = read_memory_raw(process_handle, base_address as u64, size_of::<WerContext>())?;

    if buffer.len() != size_of::<WerContext>() {
        return Err(anyhow!("Failed to read the full WerContext, wrong size"));
    }

    // Create a MaybeUninit to hold the WerContext
    let mut wer_context = MaybeUninit::<WerContext>::uninit();

    // Copy the raw bytes from the Vec<u8> into the MaybeUninit<WerContext>
    let ptr = wer_context.as_mut_ptr();
    std::ptr::copy_nonoverlapping(buffer.as_ptr(), ptr as *mut u8, buffer.len());

    // Validate prefix and suffix
    let raw_ptr = wer_context.as_ptr();

    // We can't call assume_init yet because ptr hasn't been set,
    // and apparently (*raw_ptr).prefix before calling assume_init is undefined behavior
    let prefix = read_unaligned(addr_of!((*raw_ptr).prefix));
    let suffix = read_unaligned(addr_of!((*raw_ptr).suffix));

    if prefix != WER_CONTEXT_PREFIX || suffix != WER_CONTEXT_SUFFIX {
        return Err(anyhow!("Invalid WER context"));
    }

    let ptr = read_unaligned(addr_of!((*raw_ptr).ptr));
    let len = read_unaligned(addr_of!((*raw_ptr).len));

    if ptr.is_null() || len == 0 {
        return Err(anyhow!("Invalid WER context"));
    }

    // Read the memory in the target process pointed by ptr
    let buffer = read_memory_raw(process_handle, ptr as u64, len)?;

    // Copy the buffer into a new Box<[u8]>
    let boxed_slice = buffer.into_boxed_slice();

    // Leak the Box<[u8]> to get a static reference
    let static_slice = Box::leak(boxed_slice);

    // Create a new WerContext with the leaked static reference
    let mut wer_context = WerContext {
        prefix: WER_CONTEXT_PREFIX,
        ptr: static_slice.as_ptr(),
        len: len,
        suffix: WER_CONTEXT_SUFFIX
    };

    Ok(wer_context)
}

pub unsafe fn walk_thread_stack(process_handle: HANDLE, thread_id: u32, modules: &Vec<ModuleInfo>) -> Result<StackTrace> {
    let mut stacktrace = StackTrace::new_incomplete();

    let thread_handle = OpenThread(THREAD_ALL_ACCESS, false, thread_id)?;

    let mut context = CONTEXT::default();
    context.ContextFlags = CONTEXT_FULL_AMD64;
    GetThreadContext(thread_handle, &mut context)?;

    #[cfg(target_arch = "x86_64")]
    let mut native_frame = STACKFRAME_EX::default();
    native_frame.AddrPC.Offset = context.Rip as u64;
    native_frame.AddrPC.Mode = AddrModeFlat;
    native_frame.AddrStack.Offset = context.Rsp as u64;
    native_frame.AddrStack.Mode = AddrModeFlat;
    native_frame.AddrFrame.Offset = context.Rbp as u64;
    native_frame.AddrFrame.Mode = AddrModeFlat;

    loop {
        let result = StackWalkEx(
            IMAGE_FILE_MACHINE_AMD64.0 as u32,
            process_handle,
            thread_handle,
            &mut native_frame,
            &mut context as *mut _ as *mut c_void,
            None,
            None,
            None,
            None,
            SYM_STKWALK_DEFAULT,
        );

        if result != TRUE {
            break;
        }

        let mut frame = StackFrame::new();

        frame.ip = Some(format!("{:x}", native_frame.AddrPC.Offset));
        frame.sp = Some(format!("{:x}", native_frame.AddrStack.Offset));

        // Find the module
        let module = modules.iter().find(|module| {
            module.base_address <= native_frame.AddrPC.Offset && native_frame.AddrPC.Offset < module.base_address + module.size
        });

        if let Some(module) = module {
            frame.module_base_address = Some(format!("{:x}", module.base_address));
            frame.symbol_address = Some(format!("{:x}", native_frame.AddrPC.Offset - module.base_address));
            frame.path = module.path.clone();

            if let Some(pdb_info) = &module.pdb_info {
                frame.build_id = Some(format!("{:x}{:x}", pdb_info.signature, pdb_info.age));
                frame.build_id_type = Some(datadog_crashtracker::BuildIdType::PDB);
                frame.file_type = Some(datadog_crashtracker::FileType::PDB);
                frame.build_id_type = Some(datadog_crashtracker::BuildIdType::PDB);
            }
        }

        stacktrace.push_frame(frame, true).expect("Failed to add frame");
    }

    stacktrace.set_complete().expect("Failed to set complete");

    stacktrace.set_complete()?;
    Ok(stacktrace)
}

struct ModuleInfo {
    base_address: u64,
    size: u64,
    path: Option<String>,
    pdb_info: Option<PdbInfo>
}

struct PdbInfo {
    age: u32,
    signature: GUID,
}

pub unsafe fn list_modules(process_handle: HANDLE) -> anyhow::Result<Vec<ModuleInfo>> {
    // Use EnumProcessModules to get a list of modules
    let mut module_infos = Vec::new();

    // Get the number of bytes required to store the array of module handles
    let mut cb_needed = 0;
    if !EnumProcessModules(
        process_handle,
        std::ptr::null_mut(),
        0,
        &mut cb_needed,
    ).is_ok()
    {
        return Err(anyhow!("Failed to get module list size"));
    }

    // Allocate enough space for the module handles
    let modules_count = cb_needed as usize / size_of::<HMODULE>();
    let mut hmodules: Vec<HMODULE> = Vec::with_capacity(modules_count);
    let mut cb_actual = 0;

    if !EnumProcessModules(
        process_handle,
        hmodules.as_mut_ptr(),
        cb_needed,
        &mut cb_actual,
    ).is_ok()
    {
        return Err(anyhow!("Failed to enumerate process modules"));
    }

    hmodules.set_len(cb_actual as usize / size_of::<HMODULE>());

    // Iterate through the module handles and retrieve information
    for &hmodule in hmodules.iter() {
        let mut module_info = MODULEINFO::default();
        if GetModuleInformation(
            process_handle,
            hmodule,
            &mut module_info,
            size_of::<MODULEINFO>() as u32,
        ).is_err() {
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

    return Ok(module_infos);
}

unsafe fn read_memory<T>(process_handle: HANDLE, address: u64) -> Result<T> {
    let mut bytes_read = 0;
    let mut value = MaybeUninit::<T>::uninit();
    let size = size_of::<T>();

    let result = ReadProcessMemory(
        process_handle,
        address as *const _,
        value.as_mut_ptr() as *mut _,
        size,
        Some(&mut bytes_read));

    if result.is_err() || bytes_read != size {
        return Err(anyhow!("Failed to read memory"));
    }

    Ok(value.assume_init())
}

unsafe fn read_memory_raw(process_handle: HANDLE, address: u64, size: usize) -> Result<Vec<u8>> {
    let mut buffer = vec![0u8; size];
    let mut bytes_read = 0;

    let result = ReadProcessMemory(
        process_handle,
        address as *const _,
        buffer.as_mut_ptr() as *mut _,
        size,
        Some(&mut bytes_read));

    if result.is_err() || bytes_read != size {
        return Err(anyhow!("Failed to read memory"));
    }

    Ok(buffer)
}

#[repr(C)]
pub struct IMAGE_NT_HEADERS_GENERIC {
    pub signature: u32,
    pub file_header: IMAGE_FILE_HEADER,
    pub magic: IMAGE_OPTIONAL_HEADER_MAGIC
}

#[repr(C)]
#[derive(Debug)]
pub struct GUID {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl fmt::LowerHex for GUID {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{:08x}{:04x}{:04x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.data1,
            self.data2,
            self.data3,
            self.data4[0], self.data4[1],
            self.data4[2], self.data4[3],
            self.data4[4], self.data4[5],
            self.data4[6], self.data4[7]
        )
    }
}

#[repr(C)]
pub struct CV_INFO_PDB70 {
    pub signature: u32,
    pub guid: GUID,
    pub age: u32,
}

unsafe fn get_pdb_info(process_handle: HANDLE, base_address: u64) -> Result<PdbInfo> {
    let dos_header: IMAGE_DOS_HEADER = read_memory(process_handle, base_address)?;

    if dos_header.e_magic != IMAGE_DOS_SIGNATURE {
        return Err(anyhow!("Invalid DOS header"));
    }

    let nt_headers_address = base_address + dos_header.e_lfanew as u64;
    let nt_headers: IMAGE_NT_HEADERS_GENERIC = read_memory(process_handle, nt_headers_address)?;

    if nt_headers.signature != IMAGE_NT_SIGNATURE {
        return Err(anyhow!("Invalid NT headers"));
    }

    let is_pe64 = nt_headers.file_header.Machine == IMAGE_FILE_MACHINE_AMD64;
    let is_pe32 = nt_headers.file_header.Machine == IMAGE_FILE_MACHINE_I386;

    if !is_pe32 && !is_pe64 {
        return Err(anyhow!("Invalid machine type"));
    }

    let debug_data_dir: IMAGE_DATA_DIRECTORY;

    if is_pe32 {
        let nt_headers32: IMAGE_NT_HEADERS32 = read_memory(process_handle, nt_headers_address)?;
        debug_data_dir = nt_headers32.OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_DEBUG.0 as usize];
    }
    else {
        let nt_headers64: IMAGE_NT_HEADERS64 = read_memory(process_handle, nt_headers_address)?;
        debug_data_dir = nt_headers64.OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_DEBUG.0 as usize];
    }

    let debug_dir_buffer = read_memory_raw(
        process_handle,
        base_address + debug_data_dir.VirtualAddress as u64,
        debug_data_dir.Size as usize
    )?;

    let debug_dir = std::slice::from_raw_parts(
        debug_dir_buffer.as_ptr() as *const IMAGE_DEBUG_DIRECTORY,
        debug_dir_buffer.len() / std::mem::size_of::<IMAGE_DEBUG_DIRECTORY>());

    for entry in debug_dir {
        if entry.Type != IMAGE_DEBUG_TYPE_CODEVIEW {
            continue;
        }

        let cv_info: CV_INFO_PDB70 = read_memory(process_handle, base_address + entry.AddressOfRawData as u64)?;

        if cv_info.signature == 0x53445352 /* 'RSDS' */ {
            return Ok(PdbInfo {
                age: cv_info.age,
                signature: cv_info.guid
            });
        }
    }

    Err(anyhow!("No CodeView entry found"))
}

unsafe fn get_module_path(process_handle: HANDLE, module_handle: HMODULE) -> anyhow::Result<String> {
    let mut module_name_buffer = vec![0u16; 1024];

    let len = GetModuleFileNameExW(
        Some(process_handle),
        Some(module_handle),
        &mut *module_name_buffer
    );
    if len <= 0 {
        return Err(anyhow!("GetModuleFileNameExW failed: {}", len));
    }

    let module_name = OsString::from_wide(&module_name_buffer[..len as usize])
        .to_string_lossy()
        .into_owned();

    return Ok(module_name);
}

pub unsafe fn list_threads(pid: u32) -> anyhow::Result<Vec<u32>> {
    let mut thread_ids = Vec::new();

    let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, pid)?;

    let mut thread_entry = THREADENTRY32::default();
    thread_entry.dwSize = size_of::<THREADENTRY32>() as u32;

    if Thread32First(snapshot, &mut thread_entry).is_ok() {
        loop {
            if thread_entry.th32OwnerProcessID == pid {
                thread_ids.push(thread_entry.th32ThreadID);
            }

            if !Thread32Next(snapshot, &mut thread_entry).is_ok() {
                break;
            }
        }
    }

    return Ok(thread_ids);
}