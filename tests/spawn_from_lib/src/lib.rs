// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;

use spawn_worker::SpawnWorker;
use spawn_worker::{entrypoint, TrampolineData};

#[cfg(not(target_os = "windows"))]
use spawn_worker::recv_passed_fd;

#[no_mangle]
pub extern "C" fn exported_entrypoint(_trampoline_data: &TrampolineData) {
    println!("stdout_works_as_expected");
    eprintln!("stderr_works_as_expected");
    #[cfg(not(target_os = "windows"))]
    if let Some(fd) = recv_passed_fd() {
        let mut shared_file: std::fs::File = fd.into();
        writeln!(shared_file, "shared_file_works_as_expected").unwrap();
    }
    std::io::stdout().flush().unwrap();
    std::io::stderr().flush().unwrap();
}

pub fn build() -> SpawnWorker {
    #[allow(unused_unsafe)]
    let mut worker = unsafe { SpawnWorker::new() };

    worker.target(entrypoint!(exported_entrypoint));

    worker
}
