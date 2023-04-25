// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::path::Path;
use std::process;
use sidecar_mockgen::generate_mock_symbols;

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() < 3 {
        eprintln!("Needs at least 2 args: the shared object file followed by at least one object file");
        process::exit(1);
    }

    let binary_path = Path::new(&args[1]);
    let object_paths: Vec<_> = args.iter().skip(2).map(Path::new).collect();
    match generate_mock_symbols(binary_path, object_paths.as_slice()) {
        Ok(symbols) => print!("{}", symbols),
        Err(err) => {
            eprintln!("{}", err);
            process::exit(1);
        }
    }
}
