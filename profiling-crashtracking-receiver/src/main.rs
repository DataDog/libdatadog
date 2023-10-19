use chrono::Utc;
//use clap::Parser;
use datadog_profiling::exporter::{self, Tag};
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use uuid::Uuid;

use datadog_profiling::crashtracker::*;

fn _print_to_file(data: &[u8]) -> anyhow::Result<()> {
    let now = Utc::now().to_rfc3339();
    let path = format!("{now}.txt");
    let path = Path::new(&path);
    let mut file = File::create(path)?;
    file.write_all(data)?;
    Ok(())
}

fn upload_to_dd(data: &[u8]) -> anyhow::Result<hyper::Response<hyper::Body>> {
    //let site = "intake.profile.datad0g.com/api/v2/profile";
    let site = "datad0g.com";
    let api_key = std::env::var("DD_API_KEY")?;
    let endpoint = exporter::config::agentless(site, api_key)?;
    let profiling_library_name = "dd_trace_py";
    let profiling_library_version = "1.2.3";
    let family = "";
    let tag = match Tag::new("service", "local-crash-test-upload") {
        Ok(tag) => tag,
        Err(e) => anyhow::bail!("{}", e),
    };
    let tags = Some(vec![tag]);
    let time = Utc::now();
    let timeout = std::time::Duration::from_secs(30);
    let crash_file = exporter::File {
        name: "crash-info.json",
        bytes: data,
    };
    let exporter = exporter::ProfileExporter::new(
        profiling_library_name,
        profiling_library_version,
        family,
        tags,
        endpoint,
    )?;
    let request = exporter.build(time, time, &[crash_file], &[], None, None, None, timeout)?;
    let response = exporter.send(request, None)?;
    Ok(response)
}

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
                let (_, filename) = line.split_once(" ").unwrap_or(("", "MISSING_FILENAME"));
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

#[cfg(target_os = "linux")]
fn _try_to_print_stacktrace() -> anyhow::Result<()> {
    if std::env::args().count() > 1 {
        // Child
        let ppid = std::os::unix::process::parent_id();
        let cpid = std::process::id();
        println!("Child {ppid} {cpid}");
        _emit_file(&format!("/proc/{ppid}/stack"))?;
    } else {
        // parent
        let exe = std::env::current_exe()?;
        let mut child = std::process::Command::new(exe).arg("child").spawn()?;
        let cpid = child.id();
        let ppid = std::process::id();
        println!("parent {ppid} {cpid}");
        set_ptracer(cpid)?;
        child.wait()?;
    }
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

fn _emit_file(filename: &str) -> anyhow::Result<()> {
    println!("printing {filename}");
    let file = File::open(filename)?;
    println!("{file:?}");
    let reader = std::io::BufReader::new(file);

    for line in reader.lines() {
        let line = line?;
        println!("{line}");
    }
    println!("printed {filename}");

    Ok(())
}

//https://github.com/sfackler/rstack/blob/master/rstack-self/src/lib.rs
#[cfg(target_os = "linux")]
fn set_ptracer(pid: u32) -> anyhow::Result<()> {
    use libc::{c_ulong, getppid, prctl, PR_SET_PTRACER};
    unsafe {
        let r = prctl(PR_SET_PTRACER, pid as c_ulong, 0, 0, 0);
        anyhow::ensure!(r == 0, std::io::Error::last_os_error());
    }
    Ok(())
}
