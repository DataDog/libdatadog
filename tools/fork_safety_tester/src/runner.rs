use std::{
    io,
    process::{Command, ExitStatus, Stdio},
    thread::{self, JoinHandle},
};

pub fn spawn_subprocess(
    process: &String,
    args: &[String],
) -> anyhow::Result<JoinHandle<io::Result<ExitStatus>>> {
    // eprintln!("executing subprocess: {} {}", process, args.join(" "));

    let mut child = Command::new(process)
        .args(args)
        .stdin(Stdio::null())
        .spawn()?;

    let join_handle = thread::spawn(move || child.wait());

    Ok(join_handle)
}
