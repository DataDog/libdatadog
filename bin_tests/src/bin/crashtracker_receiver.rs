// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(not(unix))]
fn main() {}

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    let replace_stack = std::env::var_os("DD_TEST_REPLACE_STACKTRACE").is_some();
    let zero_unwind_ip = std::env::var_os("DD_TEST_ZERO_UNWIND_IP").is_some();
    if replace_stack || zero_unwind_ip {
        return receive_rewritten_report(replace_stack, zero_unwind_ip);
    }
    libdd_crashtracker::receiver_entry_point_stdin()
}

/// Keep the live connection while forcing primary stack promotion or unwind retries.
#[cfg(unix)]
fn receive_rewritten_report(replace_stack: bool, zero_unwind_ip: bool) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::io::{BufRead, Write};

    const BEGIN_UCONTEXT: &str = "DD_CRASHTRACK_BEGIN_UCONTEXT";
    const BEGIN_STACKTRACE: &str = "DD_CRASHTRACK_BEGIN_STACKTRACE";
    const END_STACKTRACE: &str = "DD_CRASHTRACK_END_STACKTRACE";
    const DONE: &str = "DD_CRASHTRACK_DONE";
    const SENTINEL_FRAME: &str = "{\"function\":\"collector_fallback_sentinel\"}";

    // The collector waits for this connection to close. Reading until EOF
    // would deadlock; retain the original stdin handle through symbolization.
    let stdin = std::io::stdin();
    let mut source = stdin.lock();
    let mut rewritten = Vec::new();
    let mut in_stacktrace = false;
    let mut rewrite_ucontext = false;
    let mut rewrote_ucontext = false;
    let mut replacements = 0;

    loop {
        let mut line = String::new();
        anyhow::ensure!(
            source.read_line(&mut line)? != 0,
            "report ended before DONE"
        );
        if rewrite_ucontext {
            let mut context: libdd_crashtracker::Ucontext = serde_json::from_str(&line)?;
            let ip_register = match context.arch.as_str() {
                "x86_64" => "rip",
                "aarch64" => "pc",
                arch => anyhow::bail!("unsupported ucontext architecture {arch}"),
            };
            *context
                .registers
                .get_mut(ip_register)
                .context("missing saved instruction pointer")? = "0x0".to_string();
            writeln!(rewritten, "{}", serde_json::to_string(&context)?)?;
            rewrite_ucontext = false;
            rewrote_ucontext = true;
            continue;
        }

        let marker = line.trim_end_matches(['\r', '\n']);
        match marker {
            BEGIN_UCONTEXT if zero_unwind_ip => {
                rewrite_ucontext = true;
                rewritten.write_all(line.as_bytes())?;
            }
            BEGIN_STACKTRACE if replace_stack => {
                anyhow::ensure!(!in_stacktrace, "nested stacktrace block");
                in_stacktrace = true;
                replacements += 1;
                rewritten.write_all(line.as_bytes())?;
                writeln!(rewritten, "{SENTINEL_FRAME}")?;
            }
            END_STACKTRACE if replace_stack => {
                anyhow::ensure!(in_stacktrace, "unexpected end of stacktrace block");
                in_stacktrace = false;
                rewritten.write_all(line.as_bytes())?;
            }
            _ if !in_stacktrace => rewritten.write_all(line.as_bytes())?,
            _ => {}
        }

        if marker == DONE {
            break;
        }
    }

    anyhow::ensure!(!in_stacktrace, "unterminated stacktrace block");
    anyhow::ensure!(
        replacements == usize::from(replace_stack),
        "unexpected number of replaced stacktrace blocks: {replacements}"
    );
    anyhow::ensure!(rewrote_ucontext == zero_unwind_ip, "missing ucontext");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let stream = tokio::io::BufReader::new(std::io::Cursor::new(rewritten));
    runtime.block_on(libdd_crashtracker::async_receiver_entry_point_stream(
        stream,
        libdd_crashtracker::ReceiverFileAccess::Trusted,
    ))?;
    Ok(())
}
