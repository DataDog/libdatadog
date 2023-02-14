// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::io::Write;

#[no_mangle]
pub extern "C" fn exported_entrypoint() {
    print!("stdout_works_as_expected");
    eprint!("stderr_works_as_expected");
    std::io::stdout().flush().unwrap();
    std::io::stderr().flush().unwrap();
}
