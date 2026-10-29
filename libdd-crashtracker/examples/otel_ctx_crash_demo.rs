// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Demo: crash with an OTel thread context attached.
//!
//! Sets up the crashtracker, attaches a known trace/span context to the current
//! thread, then triggers a null-pointer dereference. The crash report is written
//! to `./crash_report.json`.
//!
//! Usage:
//!   cargo run -p libdd-crashtracker --example otel_ctx_crash_demo \
//!       --features otel-thread-ctx

#[cfg(not(all(unix, target_os = "linux")))]
fn main() {
    eprintln!("This demo only runs on Linux.");
}

#[cfg(all(unix, target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    use core::time::Duration;
    use libdd_crashtracker::{
        self as crashtracker, CrashtrackerConfiguration, CrashtrackerReceiverConfig, Metadata,
    };
    use libdd_otel_thread_ctx::linux::OwnedThreadContext;
    use std::env;
    use std::path::PathBuf;

    // Resolve the receiver binary. It's built alongside this example.
    let receiver_bin = env::current_exe()?
        .parent()
        .and_then(|p| p.parent()) // go up from examples/ to target/debug/
        .map(|p| p.join("crashtracker-receiver"))
        .expect("could not resolve receiver binary path");

    if !receiver_bin.exists() {
        eprintln!(
            "Receiver binary not found at {}.\n\
             Build it first: cargo build -p libdd-crashtracker --bin crashtracker-receiver",
            receiver_bin.display()
        );
        std::process::exit(1);
    }

    let output_dir = PathBuf::from(".");
    let crash_report = output_dir.join("crash_report.json");
    let stderr_file = output_dir.join("crash_stderr.log");

    eprintln!(
        "Crash report will be written to: {}",
        crash_report.display()
    );
    eprintln!("Receiver binary: {}", receiver_bin.display());

    // --- Set up the crashtracker ---
    let config = CrashtrackerConfiguration::builder()
        .create_alt_stack(true)
        .use_alt_stack(true)
        .endpoint_url(&format!("file://{}", crash_report.display()))
        .resolve_frames(crashtracker::StacktraceCollection::EnabledWithSymbolsInReceiver)
        .demangle_names(true)
        .signals(crashtracker::default_signals())
        .timeout(Duration::from_secs(10))
        .build()?;

    let metadata = Metadata {
        library_name: "otel-ctx-crash-demo".to_owned(),
        library_version: "0.1.0".to_owned(),
        family: "native".to_owned(),
        tags: vec![],
    };

    crashtracker::init(
        config,
        CrashtrackerReceiverConfig::new(
            vec![],
            env::vars().collect(),
            receiver_bin.to_string_lossy().to_string(),
            Some(stderr_file.to_string_lossy().to_string()),
            None,
        )?,
        metadata,
    )?;

    // --- Attach an OTel thread context ---
    let trace_id: [u8; 16] = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, //
        0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10,
    ];
    let span_id: [u8; 8] = [0xaa, 0xbb, 0xcc, 0xdd, 0x11, 0x22, 0x33, 0x44];
    let local_root_span_id: [u8; 8] = [0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe];
    let trace_flags: u8 = 0x01; // sampled

    eprintln!("Attaching OTel thread context:");
    eprintln!("  trace_id:  0123456789abcdeffedcba9876543210");
    eprintln!("  span_id:   aabbccdd11223344");
    eprintln!("  flags:     0x01 (sampled)");

    let attrs: &[(u8, &str)] = &[(1, "GET"), (2, "/api/v1/traces")];
    let ctx = OwnedThreadContext::new(trace_id, span_id, trace_flags, local_root_span_id, attrs);
    let _prev = ctx.attach();

    // Mark that we're "doing work".
    crashtracker::begin_op(crashtracker::OpTypes::ProfilerCollectingSample)?;

    eprintln!("Crashing now...");

    // --- Crash ---
    unsafe {
        #[cfg(target_arch = "x86_64")]
        std::arch::asm!("mov eax, [0]", options(nostack));

        #[cfg(target_arch = "aarch64")]
        std::arch::asm!("mov x0, #0", "ldr x1, [x0]", options(nostack));
    }

    unreachable!()
}
