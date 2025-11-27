// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Windows crash tracking test binary.
//! This binary initializes Windows crash tracking and triggers various types of crashes.

#[cfg(not(windows))]
fn main() {
    eprintln!("This binary is Windows-only");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    windows::main()
}

#[cfg(windows)]
mod windows {
    use anyhow::{Context, Result};
    use bin_tests::test_types_windows::WindowsCrashType;
    use libdd_common::Endpoint;
    use libdd_crashtracker::Metadata;
    use std::env;
    use std::path::Path;
    use windows::Win32::Foundation::{HANDLE, NTSTATUS};
    use windows::Win32::System::Diagnostics::Debug::{
        SetUnhandledExceptionFilter, EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS,
    };
    use windows::Win32::System::Threading::{
        OpenEventW, SetEvent, WaitForSingleObject, SYNCHRONIZATION_ACCESS_RIGHTS,
    };

    // Windows access rights constants
    // Note: These are defined here instead of importing from windows-rs because:
    // 1. SYNCHRONIZE and EVENT_MODIFY_STATE are in Win32::Security, not Win32::System::Threading
    // 2. Importing from Win32::Security would require additional feature flags
    // 3. These are standard Windows constants that don't change across Windows versions
    // Values are from: https://learn.microsoft.com/en-us/windows/win32/sync/synchronization-object-security-and-access-rights
    const SYNCHRONIZE: SYNCHRONIZATION_ACCESS_RIGHTS = SYNCHRONIZATION_ACCESS_RIGHTS(0x00100000); // Standard access right to synchronize with an object
    const EVENT_MODIFY_STATE: SYNCHRONIZATION_ACCESS_RIGHTS = SYNCHRONIZATION_ACCESS_RIGHTS(0x0002); // Event-specific access right to modify event state

    // Global state for WER simulator
    static mut CRASH_EVENT_HANDLE: Option<HANDLE> = None;
    static mut DONE_EVENT_HANDLE: Option<HANDLE> = None;
    static mut SIMULATOR_TIMEOUT_MS: u32 = 5000; // Default: 5 seconds
    static mut EXCEPTION_CODE: NTSTATUS = NTSTATUS(0); // Store the exception code for the simulator to read

    /// Minimal exception handler - signal crash and wait for completion (NO ALLOCATIONS!)
    ///
    /// CRITICAL: This handler keeps the crash process ALIVE while the simulator reads our memory!
    /// Flow:
    /// 1. We signal crash_event → Simulator wakes up
    /// 2. We wait on done_event → Process stays alive, memory intact
    /// 3. Simulator opens our process handle with PROCESS_VM_READ
    /// 4. Simulator uses ReadProcessMemory to read WERCONTEXT from our address space
    /// 5. Simulator signals done_event → We wake up
    /// 6. We return, process terminates
    unsafe extern "system" fn exception_handler(exception_info: *const EXCEPTION_POINTERS) -> i32 {
        // Extract the exception code (NTSTATUS) from EXCEPTION_POINTERS
        // SAFETY: Windows guarantees exception_info is valid during exception handling
        if !exception_info.is_null() {
            let exception_record = (*exception_info).ExceptionRecord;
            if !exception_record.is_null() {
                EXCEPTION_CODE = (*exception_record).ExceptionCode;
            }
        }

        // Signal the WER simulator that a crash occurred
        if let Some(crash_event) = CRASH_EVENT_HANDLE {
            let _ = SetEvent(crash_event);

            // Wait for the simulator to finish processing (with timeout)
            // IMPORTANT: This keeps the process alive so simulator can read our memory!
            if let Some(done_event) = DONE_EVENT_HANDLE {
                // Wait for the simulator to complete (configurable timeout)
                // Using WaitForSingleObject is OK here (kernel call, no heap allocation)
                let timeout_ms = SIMULATOR_TIMEOUT_MS;
                let result = WaitForSingleObject(done_event, timeout_ms);

                match result.0 {
                    0 => {
                        // WAIT_OBJECT_0: Simulator completed successfully
                        // (no message here - can't allocate for eprintln!)
                    }
                    0x102 => {
                        // WAIT_TIMEOUT: Simulator didn't complete in time
                        // Process will terminate anyway
                    }
                    _ => {
                        // WAIT_FAILED or other error
                        // Process will terminate anyway
                    }
                }
            }
        }

        // Allow process to terminate
        EXCEPTION_CONTINUE_SEARCH
    }

    /// Initialize crash handler: set up WER context and open events
    fn init_crash_handler(
        output_url: &str,
        metadata: &Metadata,
        crash_event_name: &str,
        done_event_name: &str,
        ready_event_name: &str,
        info_file: &Path,
    ) -> Result<()> {
        eprintln!("[CRASH_BINARY] Initializing crash handler...");

        // 1. Create error context and set in libdd-crashtracker
        let endpoint = if output_url.is_empty() {
            None
        } else {
            Some(Endpoint::from_slice(output_url))
        };

        let error_context = libdd_crashtracker::ErrorContext::new(endpoint, metadata.clone());
        let error_context_json = serde_json::to_string(&error_context)?;

        // Store context using libdd-crashtracker's function
        libdd_crashtracker::set_error_context(&error_context_json)?;

        // Get the address of WERCONTEXT in our (crash binary's) address space
        let context_addr = libdd_crashtracker::get_wer_context_address();

        eprintln!("[CRASH_BINARY] WER context address: {:#x}", context_addr);
        eprintln!("[CRASH_BINARY] Note: This address is in OUR address space");
        eprintln!("[CRASH_BINARY] Simulator will read it via ReadProcessMemory when we crash");

        // 2. Open named events (created by test runner)
        eprintln!("[CRASH_BINARY] Opening crash event: {}", crash_event_name);
        let crash_event_handle = unsafe {
            OpenEventW(
                EVENT_MODIFY_STATE,
                false,
                &windows::core::HSTRING::from(crash_event_name),
            )
            .context("Failed to open crash event")?
        };

        eprintln!("[CRASH_BINARY] Opening done event: {}", done_event_name);
        let done_event_handle = unsafe {
            OpenEventW(
                SYNCHRONIZE,
                false,
                &windows::core::HSTRING::from(done_event_name),
            )
            .context("Failed to open done event")?
        };

        unsafe {
            CRASH_EVENT_HANDLE = Some(crash_event_handle);
            DONE_EVENT_HANDLE = Some(done_event_handle);
        }

        // 3. Write crash info to file for test runner
        let pid = std::process::id();
        let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
        let exception_code_addr = &raw const EXCEPTION_CODE as usize;
        let crash_info = format!(
            "{}|{}|{:#x}|{:#x}",
            pid, tid, context_addr, exception_code_addr
        );

        eprintln!(
            "[CRASH_BINARY] Writing crash info to: {}",
            info_file.display()
        );
        eprintln!(
            "[CRASH_BINARY] Crash info: PID={}, TID={}, context_addr={:#x}, exception_code_addr={:#x}",
            pid, tid, context_addr, exception_code_addr
        );

        std::fs::write(info_file, crash_info).context("Failed to write crash info file")?;

        // 4. Signal ready event to test runner
        eprintln!("[CRASH_BINARY] Opening ready event: {}", ready_event_name);
        let ready_event_handle = unsafe {
            OpenEventW(
                EVENT_MODIFY_STATE,
                false,
                &windows::core::HSTRING::from(ready_event_name),
            )
            .context("Failed to open ready event")?
        };

        eprintln!("[CRASH_BINARY] Signaling ready event...");
        unsafe {
            SetEvent(ready_event_handle).context("Failed to signal ready event")?;
        }

        eprintln!("[CRASH_BINARY] Crash handler initialized and ready!");

        Ok(())
    }

    pub fn main() -> anyhow::Result<()> {
        let mut args = env::args().skip(1);
        let output_url = args.next().context("Missing output URL argument")?;
        let output_dir = args.next().context("Missing output directory argument")?;
        let crash_type_str = args.next().context("Missing crash type argument")?;
        let timeout_ms = args
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(5000); // Default: 5 seconds
        let crash_event_name = args.next().context("Missing crash event name argument")?;
        let done_event_name = args.next().context("Missing done event name argument")?;
        let ready_event_name = args.next().context("Missing ready event name argument")?;
        let info_file = args.next().context("Missing info file path argument")?;
        anyhow::ensure!(args.next().is_none(), "Unexpected extra arguments");

        let _output_dir: &Path = output_dir.as_ref();
        let info_file: &Path = info_file.as_ref();

        // Parse crash type into enum
        let crash_type: WindowsCrashType = crash_type_str
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid crash type '{}': {}", crash_type_str, e))?;

        eprintln!("[CRASH_BINARY] Starting...");
        eprintln!("[CRASH_BINARY] Crash type: {}", crash_type);
        eprintln!("[CRASH_BINARY] Timeout: {}ms", timeout_ms);

        // Set timeout in global static
        unsafe {
            SIMULATOR_TIMEOUT_MS = timeout_ms;
        }

        let metadata = Metadata {
            library_name: "libdatadog".to_owned(),
            library_version: "1.0.0".to_owned(),
            family: "native".to_owned(),
            tags: vec![
                "service:foo".to_string(),
                "service_version:bar".to_string(),
                "runtime-id:xyz".to_string(),
                "language:native".to_string(),
                "env:test".to_string(),
            ],
        };

        // Initialize crash handler (open events, set context, write info, signal ready)
        init_crash_handler(
            &output_url,
            &metadata,
            &crash_event_name,
            &done_event_name,
            &ready_event_name,
            info_file,
        )?;

        // Register exception handler
        eprintln!("[CRASH_BINARY] Registering exception handler...");
        unsafe {
            // Cast function item to function pointer
            let handler: unsafe extern "system" fn(*const EXCEPTION_POINTERS) -> i32 =
                exception_handler;
            SetUnhandledExceptionFilter(Some(Some(handler)));
        }
        eprintln!("[CRASH_BINARY] Exception handler registered");

        // Trigger the crash
        eprintln!("[CRASH_BINARY] About to trigger crash: {}", crash_type);
        trigger_crash(crash_type)?;

        // Should not reach here (process should have crashed)
        anyhow::bail!("Process did not crash as expected")
    }

    /// Triggers various types of crashes.
    fn trigger_crash(crash_type: WindowsCrashType) -> anyhow::Result<()> {
        match crash_type {
            WindowsCrashType::AccessViolationNull => {
                // Null pointer dereference - use aligned invalid address to bypass Rust's checks
                // Rust has special handling for actual null (0x0) that triggers panic
                // Must use aligned address to avoid alignment check panic
                #[allow(clippy::cast_ptr_alignment)] // We WANT an invalid pointer to trigger crash
                #[allow(unknown_lints)] // manual_dangling_ptr only exists in nightly
                #[allow(clippy::manual_dangling_ptr)]
                // We need a specific invalid address for testing
                unsafe {
                    let ptr: *const i32 = 0x4 as *const i32; // Aligned but invalid (4-byte aligned for i32)
                    std::hint::black_box(std::ptr::read_volatile(ptr)); // Force actual read
                }
            }
            WindowsCrashType::AccessViolationRead => {
                // Read from invalid address (must be aligned for i32)
                unsafe {
                    let ptr: *const i32 = 0xDEADBEEC as *const i32; // 4-byte aligned
                    let _value = *ptr; // Should cause EXCEPTION_ACCESS_VIOLATION
                }
            }
            WindowsCrashType::AccessViolationWrite => {
                // Write to invalid address (must be aligned for i32)
                unsafe {
                    let ptr: *mut i32 = 0xDEADBEEC as *mut i32; // 4-byte aligned
                    *ptr = 42; // Should cause EXCEPTION_ACCESS_VIOLATION
                }
            }
            WindowsCrashType::DivideByZero => {
                // Integer division by zero
                // We need to use assembly to bypass Rust's panic checks
                // and trigger a real CPU exception
                #[cfg(target_arch = "x86_64")]
                unsafe {
                    std::arch::asm!(
                        "xor edx, edx", // Clear dividend high bits
                        "mov eax, 1",   // Dividend = 1
                        "xor ecx, ecx", // Divisor = 0
                        "div ecx",      // Divide by zero -> EXCEPTION_INT_DIVIDE_BY_ZERO
                        options(noreturn)
                    );
                }
                #[cfg(target_arch = "x86")]
                unsafe {
                    std::arch::asm!(
                        "xor edx, edx",
                        "mov eax, 1",
                        "xor ecx, ecx",
                        "div ecx",
                        options(noreturn)
                    );
                }
                #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
                {
                    // Fallback for other architectures (use null deref instead)
                    unsafe {
                        let ptr: *const i32 = std::ptr::null();
                        let _ = *ptr;
                    }
                }
            }
            WindowsCrashType::StackOverflow => {
                // Infinite recursion to cause stack overflow
                // The large array ensures we hit the guard page quickly
                #[allow(unconditional_recursion)]
                fn recurse(_depth: u32) {
                    // Prevent the compiler from optimizing away the recursion
                    let large_array = [std::hint::black_box(0u8); 10240]; // 10KB per frame
                                                                          // Use the array to prevent it from being optimized away
                    std::hint::black_box(&large_array);
                    // Recurse deeper (depth param prevents tail call optimization)
                    recurse(_depth + 1);
                }
                recurse(0); // Should cause EXCEPTION_STACK_OVERFLOW
            }
            WindowsCrashType::IllegalInstruction => {
                // Execute illegal instruction
                #[cfg(target_arch = "x86_64")]
                unsafe {
                    std::arch::asm!("ud2"); // Undefined instruction
                }
                #[cfg(target_arch = "x86")]
                unsafe {
                    std::arch::asm!("ud2"); // Undefined instruction
                }
                #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
                {
                    // For ARM or other architectures, use abort as fallback
                    std::process::abort();
                }
            }
            WindowsCrashType::Abort => {
                // Explicit abort - use access violation to ensure WER is triggered
                // std::process::abort() may not trigger WER reliably in all configurations
                #[allow(clippy::cast_ptr_alignment)] // We WANT an invalid pointer to trigger crash
                #[allow(unknown_lints)] // manual_dangling_ptr only exists in nightly
                #[allow(clippy::manual_dangling_ptr)]
                // We need a specific invalid address for testing
                unsafe {
                    let ptr: *const u8 = 0x1 as *const u8; // u8 has 1-byte alignment, so 0x1 is fine
                    std::hint::black_box(*ptr);
                }
            }
        }

        // Should never reach here
        Ok(())
    }
}
