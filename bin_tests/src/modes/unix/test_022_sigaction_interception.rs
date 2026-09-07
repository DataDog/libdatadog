// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// Verifies that the sigaction GOT hook fires when a dynamically-loaded library
// installs a handler for a crashtracker-monitored signal after init, and that
// a telemetry warning is emitted.
//
// The test binary statically links crashtracker, so hook_symbol_excluding_self
// skips its GOT. libsigaction_caller.so is LD_PRELOAD'd so it is present
// when crashtracker::init() patches GOT entries. Calling
// dd_test_sigaction_on_sigsegv() from that library fires the hook.
//
// After the hook fires, post() restores the crashtracker's handler so the
// crash report is still generated, then polls the telemetry file until the
// sigaction_intercepted warning appears.
use libdd_crashtracker::CrashtrackerConfiguration;
use std::path::Path;

pub struct Test;

impl crate::modes::behavior::Behavior for Test {
    fn setup(
        &self,
        _output_dir: &Path,
        _config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, output_dir: &Path) -> anyhow::Result<()> {
        trigger_hook_and_restore(output_dir)
    }
}

fn trigger_hook_and_restore(output_dir: &Path) -> anyhow::Result<()> {
    let sym = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"dd_test_sigaction_on_sigsegv".as_ptr()) };
    anyhow::ensure!(
        !sym.is_null(),
        "dd_test_sigaction_on_sigsegv not found. Is libsigaction_caller.so LD_PRELOAD'd?"
    );

    type TestSigactionFn = unsafe extern "C" fn(*mut libc::sigaction) -> libc::c_int;
    let call_sigaction: TestSigactionFn =
        unsafe { std::mem::transmute::<*mut libc::c_void, TestSigactionFn>(sym) };

    // This call goes through libsigaction_caller.so's patched GOT
    // old_act receives the crashtracker's handle_posix_sigaction.
    let mut old_act: libc::sigaction = unsafe { std::mem::zeroed() };
    let ret = unsafe { call_sigaction(&mut old_act) };
    anyhow::ensure!(ret == 0, "dd_test_sigaction_on_sigsegv returned {ret}");

    // Restore the crashtracker's handler so the crash is still caught and a crash report is
    // generated.
    unsafe { libc::sigaction(libc::SIGSEGV, &old_act, std::ptr::null_mut()) };

    // Poll until the background telemetry thread writes the warning, or bail.
    let telemetry_path = output_dir.join("crash.telemetry");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(content) = std::fs::read_to_string(&telemetry_path) {
            if content.contains("sigaction_intercepted") {
                break;
            }
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "timed out waiting for sigaction_intercepted telemetry in {:?}",
            telemetry_path
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    Ok(())
}
