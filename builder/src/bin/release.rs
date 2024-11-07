// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::env;

use build_common::determine_paths;

use builder::builder::Builder;
use builder::common::Common;
#[cfg(feature = "crashtracker")]
use builder::crashtracker::CrashTracker;
#[cfg(feature = "profiling")]
use builder::profiling::Profiling;
use builder::utils::project_root;

#[derive(Debug)]
struct ReleaseArgs {
    pub out_dir: Option<String>,
    pub target: Option<String>,
}

impl From<pico_args::Arguments> for ReleaseArgs {
    fn from(mut args: pico_args::Arguments) -> Self {
        let release_args = ReleaseArgs {
            out_dir: match args.value_from_str("--out") {
                Ok(v) => Some(v),
                Err(_) => None,
            },
            target: match args.value_from_str("--target") {
                Ok(v) => Some(v),
                Err(_) => None,
            },
        };

        args.finish();
        release_args
    }
}

pub fn main() {
    let args: ReleaseArgs = pico_args::Arguments::from_env().into();

    let (_, source_path) = determine_paths();

    let profile = env::var("PROFILE").unwrap();
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let host = env::var("TARGET").unwrap();
    let out_dir = if let Some(out) = args.out_dir {
        out
    } else {
        project_root().to_string_lossy().to_string() + "/" + "release"
    };

    let target = if let Some(target) = args.target {
        target
    } else {
        host.clone()
    };

    #[allow(clippy::vec_init_then_push)]
    let features = {
        #[allow(unused_mut)]
        let mut f: Vec<String> = vec![];
        #[cfg(feature = "telemetry")]
        f.push("ddtelemetry-ffi".to_string());
        #[cfg(feature = "data-pipeline")]
        f.push("data-pipeline-ffi".to_string());
        #[cfg(feature = "crashtracker")]
        f.push("crashtracker-ffi".to_string());
        #[cfg(feature = "symbolizer")]
        f.push("symbolizer".to_string());
        f
    };

    let mut builder = Builder::new(
        source_path.to_str().unwrap(),
        &out_dir,
        &target,
        &profile,
        &features.join(","),
        &version,
    );

    builder.create_dir_structure();
    builder.add_cmake();

    // add modules based on features
    builder.add_module(Box::new(Common {
        arch: builder.arch.clone(),
        source_include: builder.source_inc.clone(),
        target_include: builder.target_include.clone(),
    }));

    #[cfg(feature = "profiling")]
    builder.add_module(Box::new(Profiling {
        arch: builder.arch.clone(),
        base_header: builder.main_header.clone(),
        features: builder.features.clone(),
        profile: builder.profile.clone(),
        source_include: builder.source_inc.clone(),
        source_lib: builder.source_lib.clone(),
        target_include: builder.target_include.clone(),
        target_lib: builder.target_lib.clone(),
        target_pkconfig: builder.target_pkconfig.clone(),
        version: builder.version.clone(),
    }));

    #[cfg(feature = "crashtracker")]
    builder.add_module(Box::new(CrashTracker {
        arch: builder.arch.clone(),
        base_header: builder.main_header.clone(),
        profile: builder.profile.clone(),
        source_include: builder.source_inc.clone(),
        target_include: builder.target_include.clone(),
        target_dir: builder.target_dir.clone(),
    }));

    // Build artifacts.
    let res = builder.build();
    match res {
        Ok(_) => {
            builder.sanitize_libraries();
            builder.pack().unwrap()
        }
        Err(err) => panic!("{}", format!("Building failed: {}", err)),
    }
}
