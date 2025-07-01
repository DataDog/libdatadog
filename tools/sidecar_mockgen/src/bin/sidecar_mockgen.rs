// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use sidecar_mockgen::generate_mock_symbols;
use std::path::Path;
use std::process;

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() < 3 {
        eprintln!(
            "Needs at least 2 args: the shared object file followed by at least one object file"
        );
        process::exit(1);
    }

    let binary_path = Path::new(&args[1]);
    let object_paths: Vec<_> = args.iter().skip(2).map(Path::new).collect();
    match generate_mock_symbols(binary_path, object_paths.as_slice()) {
        Ok(symbols) => print!("{symbols}"),
        Err(err) => {
            eprintln!("{err}");
            process::exit(1);
        }
    }
}
