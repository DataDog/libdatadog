use std::{
    io,
    process::{Command, ExitStatus, Stdio},
    thread::{self, JoinHandle},
};

use spawn_worker::WaitStatus;


pub fn spawn_subprocess(
    process: &String,
    args: &[String],
) -> anyhow::Result<JoinHandle<anyhow::Result<WaitStatus>>> {
    // eprintln!("executing subprocess: {} {}", process, args.join(" "));

    let mut child = unsafe { spawn_worker::SpawnWorker::new() }
        .target(spawn_worker::Target::External(process.to_owned(), args.to_owned()))
        .stdin(spawn_worker::Stdio::Null)
        .spawn()?;

    let join_handle = thread::spawn(move || child.wait());

    Ok(join_handle)
}
