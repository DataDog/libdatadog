// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use nix::sys::signal;

extern "C" fn handle_sigsegv(_: i32) {
    println!("foo");
}

pub fn register_crash_handler() -> anyhow::Result<()> {
    let sig_action: signal::SigAction = signal::SigAction::new(
        signal::SigHandler::Handler(handle_sigsegv),
        signal::SaFlags::SA_NODEFER,
        signal::SigSet::empty(),
    );
    unsafe {
        signal::sigaction(signal::SIGSEGV, &sig_action)?;
    }
    Ok(())
}
