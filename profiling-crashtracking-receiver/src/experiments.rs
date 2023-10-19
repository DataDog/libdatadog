// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use std::fs::File;
use std::io::BufRead;
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
