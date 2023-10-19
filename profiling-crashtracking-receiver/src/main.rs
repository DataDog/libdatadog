// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

mod experiments;
mod exporters;

//use clap::Parser;
use datadog_profiling::crashtracker::*;
use exporters::*;
use std::io::prelude::*;
use uuid::Uuid;

// #[derive(Parser, Debug)]
// struct Args {
//     #[arg(long)]
//     family: String,
//     #[arg(long)]
//     profiling_library_name: String,
//     #[arg(long)]
//     profiling_library_version: String,
// }

// The Bools track if we need a comma preceding the next item
#[derive(Debug)]
enum StdinState {
    SigInfo {
        comma_needed: bool,
    },
    StackTrace {
        comma_needed: bool,
    },
    Waiting,
    File {
        comma_needed: bool,
        filename: String,
    },
}

// // TODO, add commas as needed
fn process_line(w: &mut impl Write, line: String, state: StdinState) -> anyhow::Result<StdinState> {
    let next = match state {
        StdinState::SigInfo { comma_needed } => {
            if line.starts_with(DD_CRASHTRACK_END_SIGINFO) {
                write!(w, "\n}}")?;
                StdinState::Waiting
            } else {
                if comma_needed {
                    writeln!(w, ",")?;
                }
                write!(w, "\t{line}")?;
                StdinState::SigInfo { comma_needed: true }
            }
        }
        StdinState::Waiting => {
            if line.starts_with(DD_CRASHTRACK_BEGIN_FILE) {
                let (_, filename) = line.split_once(' ').unwrap_or(("", "MISSING_FILENAME"));
                writeln!(w, ",")?;
                writeln!(w, "{filename} : [")?;
                StdinState::File {
                    comma_needed: false,
                    filename: filename.to_string(),
                }
            } else if line.starts_with(DD_CRASHTRACK_BEGIN_STACKTRACE) {
                writeln!(w, ",")?;
                writeln!(w, "\"stacktrace\": [")?;
                StdinState::StackTrace {
                    comma_needed: false,
                }
            } else if line.starts_with(DD_CRASHTRACK_BEGIN_SIGINFO) {
                writeln!(w, ",")?;
                writeln!(w, "\"siginfo\": {{")?;
                StdinState::SigInfo {
                    comma_needed: false,
                }
            } else {
                eprint!("Unexpected line: {line}");
                StdinState::Waiting
            }
        }
        StdinState::File {
            comma_needed,
            filename,
        } => {
            if line.starts_with(DD_CRASHTRACK_END_FILE) {
                write!(w, "\n]")?;
                StdinState::Waiting
            } else {
                if comma_needed {
                    writeln!(w, ",")?;
                }
                write!(w, "\t\"{line}\"")?;
                StdinState::File {
                    comma_needed: true,
                    filename,
                }
            }
        }
        StdinState::StackTrace { comma_needed } => {
            if line.starts_with(DD_CRASHTRACK_END_STACKTRACE) {
                write!(w, "]")?;
                StdinState::Waiting
            } else {
                if comma_needed {
                    writeln!(w, ",")?;
                }
                write!(w, "\t{line}")?;
                StdinState::StackTrace { comma_needed: true }
            }
        }
    };
    Ok(next)
}

fn emit_json_prefix(w: &mut impl Write, uuid: &Uuid) -> anyhow::Result<()> {
    writeln!(w, "{{")?;
    write!(w, "\"uuid\": \"{uuid}\"")?;

    Ok(())
}

fn emit_json_suffix(w: &mut impl Write) -> anyhow::Result<()> {
    writeln!(w, "\n}}")?;

    Ok(())
}

/// Recieves data on stdin, and forwards it to somewhere its useful
/// For now, just sent to a file.
/// Future enhancement: set of key/value pairs sent over pipe to setup
/// Future enhancement: publish to DD endpoint
pub fn main() -> anyhow::Result<()> {
    let uuid = Uuid::new_v4();
    let mut buf = vec![];
    let stdin = std::io::stdin();

    emit_json_prefix(&mut buf, &uuid)?;
    let mut stdin_state = StdinState::Waiting;
    for line in stdin.lock().lines() {
        let line = line?;
        stdin_state = process_line(&mut buf, line, stdin_state)?;
    }
    emit_json_suffix(&mut buf)?;
    //std::io::stdout().write_all(&buf)?;
    _print_to_file(&buf)?;
    //upload_to_dd(&buf)?;
    Ok(())
}
