use std::{
    env,
    ffi::CStr,
    fmt::format,
    io,
    process::{Command, ExitStatus, Stdio},
    thread::{self, JoinHandle},
};

use nix::libc::RTLD_LAZY;

const ARGS_SPLITTER: &str = "--";

fn do_spawn_subprocess(
    process: &String,
    args: &[String],
) -> anyhow::Result<JoinHandle<io::Result<ExitStatus>>> {
    eprintln!("executing subprocess: {} {}", process, args.join(" "));

    let mut child = Command::new(process)
        .args(args)
        .stdin(Stdio::null())
        .spawn()?;

    let join_handle = thread::spawn(move || child.wait());

    Ok(join_handle)
}

fn spawn_subprocess(
    sub_process: Option<&[String]>,
) -> anyhow::Result<JoinHandle<io::Result<ExitStatus>>> {
    let process_join = match sub_process {
        Some(sub_args) if sub_args.len() > 0 => do_spawn_subprocess(&sub_args[0], &sub_args[1..])?,
        _ => do_spawn_subprocess(&"bash".into(), &["-c".into(), "sleep 1".into()])?,
    };

    Ok(process_join)
}

fn perform_allocation() {
    let expected_size = 1 * 1024 * 1024;
    let mut buffer = vec![0_u8; expected_size];
    buffer[0] = 0xff;
    buffer[expected_size - 1] = 0xff;
    assert_eq!(buffer.len(), expected_size);
}

fn spawn_continous_action<F: Fn() + Send + 'static>(
    name: &str,
    id: usize,
    status_every: usize,
    f: F,
) {
    let name = format!("{name}[{id}]");
    thread::spawn(move || {
        let mut iterations = 1;
        loop {
            f();
            iterations += 1;
            if iterations % status_every == 0 {
                eprintln!("{}: {}", name, iterations);
            }
        }
    });
}

fn perform_dlopen() {
    unsafe {
        let handle = nix::libc::dlopen(
            CStr::from_bytes_with_nul_unchecked(b"libsystemd.so.0\0").as_ptr(),
            RTLD_LAZY,
        );
        if handle.is_null() {
            panic!("failed loading systemd.so")
        }
        if nix::libc::dlclose(handle) != 0 {
            panic!("failed closing handle")
        }
    }
}

fn main() {
    let args = env::args().collect::<Vec<_>>();
    let mut split_args = args.splitn(2, |arg| arg.eq(ARGS_SPLITTER));
    let tester_args = split_args.next().unwrap();

    let sub_process = split_args.next();

    // for id in 0..8 {
    //     spawn_continous_action("allocations", id, 10000, perform_allocation);
    // }

    for id in 0..8 {
        spawn_continous_action("dlopen", id, 1000, perform_dlopen);
    }

    let exit_code = spawn_subprocess(sub_process)
        .unwrap()
        .join()
        .unwrap()
        .unwrap();

    match exit_code.code() {
        Some(code) => eprintln!("process exited with: {}", code),
        None => eprintln!("process terminated via signal"),
    }
}
