// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! WER Simulator: Out-of-process crash handler for testing.
//! Mimics WER's out-of-process mechanism without requiring WER service.
//!
//! This is designed for CI and local testing environments only.
//! For production, use WER's native WerRegisterRuntimeExceptionModule.

// Stub main for non-Windows platforms (allows compilation but binary won't work)
#[cfg(not(windows))]
fn main() {
    eprintln!("wer_simulator is only available on Windows");
    std::process::exit(1);
}

#[cfg(windows)]
use {
    anyhow::{Context, Result},
    std::env,
    windows::Win32::Foundation::NTSTATUS,
    windows::Win32::System::Threading::{
        OpenEventW, OpenProcess, OpenThread, SetEvent, WaitForSingleObject, INFINITE,
        PROCESS_QUERY_INFORMATION, PROCESS_VM_READ, SYNCHRONIZATION_ACCESS_RIGHTS,
        THREAD_QUERY_INFORMATION,
    },
};

// Windows access rights constants
// Note: These are defined here instead of importing from windows-rs because:
// 1. SYNCHRONIZE and EVENT_MODIFY_STATE are in Win32::Security, not Win32::System::Threading
// 2. Importing from Win32::Security would require additional feature flags
// 3. These are standard Windows constants that don't change across Windows versions
// Values are from: https://learn.microsoft.com/en-us/windows/win32/sync/synchronization-object-security-and-access-rights
#[cfg(windows)]
const SYNCHRONIZE: SYNCHRONIZATION_ACCESS_RIGHTS = SYNCHRONIZATION_ACCESS_RIGHTS(0x00100000); // Standard access right to synchronize with an object
#[cfg(windows)]
const EVENT_MODIFY_STATE: SYNCHRONIZATION_ACCESS_RIGHTS = SYNCHRONIZATION_ACCESS_RIGHTS(0x0002); // Event-specific access right to modify event state

#[cfg(windows)]
fn main() -> Result<()> {
    eprintln!("[WER_SIMULATOR] Starting...");

    // Parse command-line arguments
    let args: Vec<String> = env::args().collect();

    eprintln!("[WER_SIMULATOR] Received {} arguments:", args.len());
    for (i, arg) in args.iter().enumerate() {
        eprintln!("[WER_SIMULATOR]   args[{}] = {}", i, arg);
    }

    if args.len() != 9 {
        anyhow::bail!(
            "Usage: wer_simulator <parent_pid> <parent_tid> <context_address> <exception_code_address> <crash_event_name> <done_event_name> <ready_event_name> <output_dir>\nReceived {} args: {:?}",
            args.len(),
            args
        );
    }

    let parent_pid: u32 = args[1].parse().context("Invalid PID")?;
    let parent_tid: u32 = args[2].parse().context("Invalid TID")?;

    // Parse context address (supports both hex "0x..." and decimal formats)
    let context_address: usize = if args[3].starts_with("0x") || args[3].starts_with("0X") {
        usize::from_str_radix(&args[3][2..], 16).context("Invalid hex context address")?
    } else {
        args[3].parse().context("Invalid decimal context address")?
    };

    // Parse exception code address (supports both hex "0x..." and decimal formats)
    let exception_code_address: usize = if args[4].starts_with("0x") || args[4].starts_with("0X") {
        usize::from_str_radix(&args[4][2..], 16).context("Invalid hex exception code address")?
    } else {
        args[4]
            .parse()
            .context("Invalid decimal exception code address")?
    };

    let crash_event_name = &args[5];
    let done_event_name = &args[6];
    let ready_event_name = &args[7];
    let output_dir = std::path::Path::new(&args[8]);

    eprintln!("[WER_SIMULATOR] Parent PID: {}", parent_pid);
    eprintln!("[WER_SIMULATOR] Parent TID: {}", parent_tid);
    eprintln!("[WER_SIMULATOR] Context address: {:#x}", context_address);
    eprintln!(
        "[WER_SIMULATOR] Exception code address: {:#x}",
        exception_code_address
    );
    eprintln!("[WER_SIMULATOR] Crash event: {}", crash_event_name);
    eprintln!("[WER_SIMULATOR] Done event: {}", done_event_name);
    eprintln!("[WER_SIMULATOR] Ready event: {}", ready_event_name);

    // Open the crash event (parent will signal this)
    eprintln!("[WER_SIMULATOR] Opening crash event...");
    let crash_event_handle = unsafe {
        OpenEventW(
            SYNCHRONIZE,
            false,
            &windows::core::HSTRING::from(crash_event_name),
        )
        .context("Failed to open crash event")?
    };

    // Open the done event (we will signal this)
    eprintln!("[WER_SIMULATOR] Opening done event...");
    let done_event_handle = unsafe {
        OpenEventW(
            EVENT_MODIFY_STATE,
            false,
            &windows::core::HSTRING::from(done_event_name),
        )
        .context("Failed to open done event")?
    };

    // Open the ready event (we will signal this to indicate we're ready)
    eprintln!("[WER_SIMULATOR] Opening ready event...");
    let ready_event_handle = unsafe {
        OpenEventW(
            EVENT_MODIFY_STATE,
            false,
            &windows::core::HSTRING::from(ready_event_name),
        )
        .context("Failed to open ready event")?
    };

    // Signal that we're ready
    eprintln!("[WER_SIMULATOR] Signaling ready event...");
    unsafe {
        SetEvent(ready_event_handle).context("Failed to signal ready event")?;
    }

    eprintln!("[WER_SIMULATOR] Ready! Waiting for crash event...");

    // Wait for the crash event to be signaled (blocking)
    unsafe {
        WaitForSingleObject(crash_event_handle, INFINITE);
    }

    eprintln!("[WER_SIMULATOR] Crash event signaled! Processing...");

    // Open parent process with read/query permissions
    // IMPORTANT: PROCESS_VM_READ allows us to read the parent's memory remotely!
    let process_handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
            false,
            parent_pid,
        )
        .context("Failed to open parent process")?
    };

    eprintln!("[WER_SIMULATOR] Opened parent process with PROCESS_VM_READ permission");

    // Open parent thread with query permissions
    let thread_handle = unsafe {
        OpenThread(THREAD_QUERY_INFORMATION, false, parent_tid)
            .context("Failed to open parent thread")?
    };

    // Read the exception code (NTSTATUS) from the parent process's memory
    eprintln!(
        "[WER_SIMULATOR] Reading exception code from parent process at address {:#x}...",
        exception_code_address
    );
    let exception_code: NTSTATUS = unsafe {
        let mut exception_code = NTSTATUS(0);
        let mut bytes_read = 0usize;
        let result = windows::Win32::System::Diagnostics::Debug::ReadProcessMemory(
            process_handle,
            exception_code_address as *const _,
            &mut exception_code as *mut _ as *mut _,
            std::mem::size_of::<NTSTATUS>(),
            Some(&mut bytes_read),
        );

        if result.is_err() || bytes_read != std::mem::size_of::<NTSTATUS>() {
            anyhow::bail!(
                "Failed to read exception code from address {:#x}",
                exception_code_address
            );
        }
        exception_code
    };
    eprintln!("[WER_SIMULATOR] Exception code: {:#x}", exception_code.0);

    // Call libdd-crashtracker's exception handler
    // NOTE: context_address is in the PARENT process's address space, not ours!
    // exception_event_callback will use ReadProcessMemory(process_handle, context_address, ...)
    // to read the WER context from the parent process's memory.
    // This works because:
    // 1. Parent process is still alive (blocked in exception handler waiting on done_event)
    // 2. We have PROCESS_VM_READ permission
    // 3. exception_event_callback knows to use cross-process memory reading
    eprintln!("[WER_SIMULATOR] Calling exception_event_callback...");
    eprintln!("[WER_SIMULATOR] Will read WER context from parent process via ReadProcessMemory");
    match libdd_crashtracker::exception_event_callback(
        context_address,
        process_handle,
        thread_handle,
        exception_code,
    ) {
        Ok(_) => {
            eprintln!("[WER_SIMULATOR] Crash report generated successfully");
            // Write success marker
            let success_file = output_dir.join("wer_simulator_success.txt");
            let _ = std::fs::write(&success_file, "SUCCESS");
            eprintln!(
                "[WER_SIMULATOR] Success marker written to: {}",
                success_file.display()
            );
        }
        Err(e) => {
            eprintln!("[WER_SIMULATOR] Error: {:?}", e);
            // Write error marker
            let error_file = output_dir.join("wer_simulator_error.txt");
            let _ = std::fs::write(&error_file, format!("{:?}", e));
            eprintln!(
                "[WER_SIMULATOR] Error marker written to: {}",
                error_file.display()
            );
        }
    }

    // Signal the parent that we're done processing
    eprintln!("[WER_SIMULATOR] Signaling completion to parent...");
    unsafe {
        SetEvent(done_event_handle).context("Failed to signal done event")?;
    }

    eprintln!("[WER_SIMULATOR] Exiting");
    Ok(())
}
