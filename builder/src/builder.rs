// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::Result;
use build_common::HEADER_PATH;
use tar;

use crate::arch;
use crate::module::Module;
use crate::utils::{file_replace, project_root};

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
/// use anyhow::Result;
/// use builder::builder::Builder;
/// use builder::module::Module;
/// use std::rc::Rc;
/// struct Core {
///     version: Rc<str>,
/// }
///
/// impl Module for Core {
///     fn build(&self) -> Result<()> {
///         Ok(())
///     }
///     fn install(&self) -> Result<()> {
///         Ok(())
///     }
/// }
/// let mut builder = Builder::new("source", "target", "arch", "features", "profile", "version");
/// let core = Box::new(Core {
///     version: builder.version.clone(),
/// });
/// builder.add_module(core);
/// builder.build();
/// ```
pub struct Builder {
    modules: Vec<Box<dyn Module>>,
    pub arch: Rc<str>,
    pub features: Rc<str>,
    pub main_header: Rc<str>,
    pub profile: Rc<str>,
    pub source_inc: Rc<str>,
    pub source_lib: Rc<str>,
    pub target_dir: Rc<str>,
    pub target_lib: Rc<str>,
    pub target_include: Rc<str>,
    pub target_bin: Rc<str>,
    pub target_pkconfig: Rc<str>,
    pub version: Rc<str>,
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
    pub fn new(
        source_dir: &str,
        target_dir: &str,
        target_arch: &str,
        profile: &str,
        features: &str,
        version: &str,
    ) -> Self {
        Builder {
            modules: Vec::new(),
            arch: target_arch.into(),
            features: features.into(),
            main_header: (target_dir.to_string() + "/" + HEADER_PATH + "/" + "common.h").into(),
            profile: profile.into(),
            source_lib: (source_dir.to_string() + "/" + target_arch + "/" + profile + "/deps")
                .into(),
            source_inc: (source_dir.to_string() + "/" + HEADER_PATH).into(),
            target_dir: target_dir.into(),
            target_lib: (target_dir.to_string() + "/" + "/lib").into(),
            target_include: (target_dir.to_string() + "/" + HEADER_PATH).into(),
            target_bin: (target_dir.to_string() + "/bin").into(),
            target_pkconfig: (target_dir.to_string() + "/lib/pkgconfig").into(),
            version: version.into(),
        }
    }

    /// Adds a boxed object which implements Module trait.
    pub fn add_module(&mut self, module: Box<dyn Module>) {
        self.modules.push(module);
    }

    pub fn create_dir_structure(&self) {
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

    // TODO: maybe do this in module's build.rs
    pub fn sanitize_libraries(&self) {
        let datadog_lib_dir = Path::new(self.source_lib.as_ref());

        let libs = fs::read_dir(datadog_lib_dir).unwrap();
        for lib in libs.flatten() {
            let name = lib.file_name().into_string().unwrap();
            if name.ends_with(".so") {
                arch::fix_rpath(lib.path().to_str().unwrap());
            }
        }
    }

    pub fn add_cmake(&self) {
        let libs = arch::NATIVE_LIBS.to_owned();
        let cmake_path: PathBuf = [&self.target_dir, "DatadogConfig.cmake"].iter().collect();
        let mut origin = project_root();
        origin.push("cmake");
        origin.push("DatadogConfig.cmake.in");

        file_replace(
            origin.to_str().unwrap(),
            cmake_path.to_str().unwrap(),
            "@Datadog_LIBRARIES@",
            libs.trim(),
        )
        .expect("Failed to modify the cmake");
    }

    /// Builds the final artifact by going through all modules and instancing their install method.
    ///
    /// #Returns
    ///
    /// Ok(()) if success Err(_) if failure.
    pub fn build(&self) -> Result<()> {
        for module in &self.modules {
            module.build()?;
            module.install()?;
        }
        Ok(())
    }

    /// Generate a tar file with all the intermediate artifacts generated by all the modules.k
    ///
    /// #Returns
    ///
    /// Ok(()) if success Err(_) if failure.
    pub fn pack(&self) -> Result<()> {
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
