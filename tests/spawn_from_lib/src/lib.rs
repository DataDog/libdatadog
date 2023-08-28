// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
// #![cfg(unix)]

use std::io::Write;

use spawn_worker::entrypoint;
use spawn_worker::SpawnWorker;

#[cfg(not(target_os = "windows"))]
use spawn_worker::recv_passed_fd;

#[no_mangle]
pub extern "C" fn exported_entrypoint() {
    println!("stdout_works_as_expected");
    eprintln!("stderr_works_as_expected");
    #[cfg(not(target_os = "windows"))]
    if let Some(fd) = recv_passed_fd() {
        let mut shared_file: File = fd.into();
        writeln!(shared_file, "shared_file_works_as_expected").unwrap();
    }
    std::io::stdout().flush().unwrap();
    std::io::stderr().flush().unwrap();
}

pub fn build() -> SpawnWorker {
    let mut worker = unsafe { SpawnWorker::new() };

    worker.target(entrypoint!(exported_entrypoint));

    worker
}
