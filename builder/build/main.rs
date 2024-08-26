// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod arch;
mod common;
#[cfg(feature = "crashtracker")]
mod crashtracker;
mod module;

#[cfg(feature = "data-pipeline")]
mod data_pipeline;

#[cfg(feature = "profiling")]
mod profiling;

#[cfg(feature = "symbolizer")]
mod symbolizer;

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::{env, fs};

use build_common::{determine_paths, HEADER_PATH};
use tools::headers::dedup_headers;

use crate::common::Common;
#[cfg(feature = "crashtracker")]
use crate::crashtracker::CrashTracker;
#[cfg(feature = "data-pipeline")]
use crate::data_pipeline::DataPipeline;
#[cfg(feature = "profiling")]
use crate::profiling::Profiling;
#[cfg(feature = "symbolizer")]
use crate::symbolizer::Symbolizer;
use module::Module;

/// [`Builder`] is a structure that holds all the information required to assemble the final
/// workspace artifact. It will manage the different modules which will be in charge of producing
/// the different binaries and source files that will be part of the artifact. The builder will
/// provide the needed information: paths, version, etc, to the different modules so they can
/// install their sub-artifacts on the target folder.
/// The target folder is set through `LIBDD_OUTPUT_FOLDER` environment variable if it is not
/// provided the default target folder will be the builder output directory.
///
/// # Example
///
/// ```rust
/// use crate::core::Core;
///
/// let mut builder = Builder::new(&path, &profile, &version);
/// let core = Box::new(Core {
///     version: builder.version.clone(),
/// });
/// builder.add_module(core);
/// builder.build()?;
/// builder.pack()?;
/// ```
struct Builder {
    modules: Vec<Box<dyn Module>>,
    main_header: Rc<str>,
    source_inc: Rc<str>,
    source_lib: Rc<str>,
    target_dir: Rc<str>,
    target_lib: Rc<str>,
    target_include: Rc<str>,
    target_bin: Rc<str>,
    target_pkconfig: Rc<str>,
    version: Rc<str>,
}

impl Builder {
    /// Creates a new Builder instance
    ///
    /// # Aguments
    ///
    /// * `target_dir`: artifact folder.
    /// * `profile`: Release configuration: debug or release;
    /// * `version`: artifact's version.
    ///
    /// # Returns
    ///
    /// A new Builder instance.
    fn new(source_dir: &str, target_dir: &str, profile: &str, version: &str) -> Self {
        Builder {
            modules: Vec::new(),
            main_header: "common.h".into(),
            source_lib: (source_dir.to_string() + "/" + profile + "/deps").into(),
            source_inc: (source_dir.to_string() + "/" + HEADER_PATH).into(),
            target_dir: target_dir.into(),
            target_lib: (target_dir.to_string() + "/lib").into(),
            target_include: (target_dir.to_string() + "/" + HEADER_PATH).into(),
            target_bin: (target_dir.to_string() + "/bin").into(),
            target_pkconfig: (target_dir.to_string() + "/lib/pkgconfig").into(),
            version: version.into(),
        }
    }

    /// Adds a boxed object which implements Module trait.
    fn add_module(&mut self, module: Box<dyn Module>) {
        self.modules.push(module);
    }

    fn create_dir_structure(&self) {
        let target = Path::new(self.target_dir.as_ref());
        if fs::metadata(target).is_ok() {
            fs::remove_dir_all(Path::new(self.target_dir.as_ref()))
                .expect("Failed to clean preexisting target folder");
        }
        fs::create_dir_all(Path::new(self.target_dir.as_ref()))
            .expect("Failed to create target directory");
        fs::create_dir_all(Path::new(self.target_include.as_ref()))
            .expect("Failed to create include directory");
        fs::create_dir_all(Path::new(self.target_lib.as_ref()))
            .expect("Failed to create include directory");
        fs::create_dir_all(Path::new(self.target_bin.as_ref()))
            .expect("Failed to create include directory");
        fs::create_dir_all(Path::new(self.target_pkconfig.as_ref()))
            .expect("Failed to create include directory");
    }

    fn deduplicate_headers(&self) {
        let datadog_inc_dir = Path::new(self.source_inc.as_ref());

        let mut headers: Vec<String> = Vec::new();
        let inc_files = fs::read_dir(datadog_inc_dir).unwrap();
        for file in inc_files.flatten() {
            let name = file.file_name().into_string().unwrap();
            if name.ends_with(".h") && !name.eq("common.h") && !name.eq("blazesym.h") {
                headers.push(file.path().to_string_lossy().to_string());
            }
        }

        let base_header = self.source_inc.to_string() + "/" + self.main_header.as_ref();
        dedup_headers(&base_header, &headers);
    }

    // TODO: maybe do this in module's build.rs
    fn sanitize_libraries(&self) {
        let datadog_lib_dir = Path::new(self.source_lib.as_ref());

        let libs = fs::read_dir(datadog_lib_dir).unwrap();
        for lib in libs.flatten() {
            let name = lib.file_name().into_string().unwrap();
            if name.ends_with(".so") {
                arch::fix_rpath(lib.path().to_str().unwrap());
            }
        }
    }

    fn add_cmake(&self) {
        let libs = arch::NATIVE_LIBS.to_owned();
        let output = Command::new("sed")
            .arg("s/@Datadog_LIBRARIES@/".to_string() + libs.trim() + "/g")
            .arg("../cmake/DatadogConfig.cmake.in")
            .output()
            .expect("Failed to modify cmake");

        let cmake_path: PathBuf = [&self.target_dir, "DatadogConfig.cmake"].iter().collect();
        fs::write(cmake_path, output.stdout).expect("writing cmake file failed");
    }

    /// Builds the final artifact by going through all modules and instancing their install method.
    ///
    /// #Returns
    ///
    /// Ok(()) if success Err(_) if failure.
    fn build(&self) -> Result<()> {
        for module in &self.modules {
            module.install()?;
        }
        Ok(())
    }

    /// Generate a tar file with all the intermediate artifacts generated by all the modules.k
    ///
    /// #Returns
    ///
    /// Ok(()) if success Err(_) if failure.
    fn pack(&self) -> Result<()> {
        let tarname = "libdatadog".to_string() + "_v" + &self.version + ".tar";
        let path: PathBuf = [self.target_dir.as_ref(), &tarname].iter().collect();
        let artifact = fs::File::create(path).expect("Failed to create tarfile");
        let mut ar = tar::Builder::new(artifact);
        ar.append_dir_all("lib", self.target_lib.as_ref())?;
        ar.append_dir("bin", self.target_bin.as_ref())?;
        ar.append_dir_all("include/datadog", self.target_include.as_ref())?;

        ar.finish().expect("Failed to write the tarfile");
        Ok(())
    }
}

fn main() {
    // Rerun build script if any of the env vars change.
    println!("cargo:rerun-if-env-changed=LIBDD_OUTPUT_FOLDER");
    println!("cargo:rerun-if-env-changed=PROFILE");

    let (_, source_path) = determine_paths();
    let mut path = env::var("OUT_DIR").unwrap();
    if let Ok(libdd_path) = env::var("LIBDD_OUTPUT_FOLDER") {
        path = libdd_path;
    }

    let profile = env::var("PROFILE").unwrap();
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut builder = Builder::new(source_path.to_str().unwrap(), &path, &profile, &version);

    builder.create_dir_structure();
    builder.deduplicate_headers();
    builder.sanitize_libraries();
    builder.add_cmake();

    // add modules based on features
    builder.add_module(Box::new(Common {
        source_include: builder.source_inc.clone(),
        target_include: builder.target_include.clone(),
    }));

    #[cfg(feature = "profiling")]
    builder.add_module(Box::new(Profiling {
        source_include: builder.source_inc.clone(),
        source_lib: builder.source_lib.clone(),
        target_include: builder.target_include.clone(),
        target_lib: builder.target_lib.clone(),
        target_pkconfig: builder.target_pkconfig.clone(),
        version: builder.version.clone(),
    }));

    #[cfg(feature = "data-pipeline")]
    builder.add_module(Box::new(DataPipeline {
        source_include: builder.source_inc.clone(),
        target_include: builder.target_include.clone(),
    }));

    #[cfg(feature = "symbolizer")]
    builder.add_module(Box::new(Symbolizer {
        source_include: builder.source_inc.clone(),
        target_include: builder.target_include.clone(),
    }));

    #[cfg(feature = "crashtracker")]
    builder.add_module(Box::new(CrashTracker {
        source_include: builder.source_inc.clone(),
        target_include: builder.target_include.clone(),
        target_dir: builder.target_dir.clone(),
    }));

    // Build artifacts.
    let res = builder.build();
    match res {
        Ok(_) => builder.pack().unwrap(),
        Err(err) => panic!("{}", format!("Building failed: {}", err)),
    }
}
