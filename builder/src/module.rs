// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;

/// The trait Module is used to handle different modules under the same interface. A [`Module`] is
/// a installation unit used by the builder to install specific artifacts pertaining to certain
/// crate belonging to the workspace.
///
/// # Examples
///
/// Assuming there is a crate inside the workspace named core-ffi and this crate produces a library
/// `libcore_ffi.so` and a header file `core-ffi.h`:
/// ```
/// use anyhow::Result;
/// use builder::module::Module;
/// use std::fs;
/// use std::path::{Path, PathBuf};
/// use std::rc::Rc;
///
/// struct Core {
///     pub source_include: Rc<str>,
///     pub source_lib: Rc<str>,
///     pub target_include: Rc<str>,
///     pub target_lib: Rc<str>,
/// }
///
/// impl Core {
///     fn add_header(&self) -> Result<()> {
///         let mut origin_path: PathBuf = [&self.source_include, "core.h"].iter().collect();
///         let mut target_path: PathBuf = [&self.target_include, "core.h"].iter().collect();
///         fs::copy(&origin_path, &target_path).expect("Failed to copy the header");
///         Ok(())
///     }
///
///     fn add_lib(&self) -> Result<()> {
///         let mut origin_path: PathBuf = [&self.source_lib, "libcore_ffi.so"].iter().collect();
///         let mut target_path: PathBuf = [&self.target_lib, "libcore_ffi.so"].iter().collect();
///         fs::copy(&origin_path, &target_path).expect("Failed to copy the library");
///         Ok(())
///     }
/// }
///
/// impl Module for Core {
///     fn build(&self) -> Result<()> {
///         Ok(())
///     }
///     fn install(&self) -> Result<()> {
///         self.add_header()?;
///         self.add_lib()?;
///         Ok(())
///     }
/// }
/// ```
pub trait Module {
    fn build(&self) -> Result<()>;
    fn install(&self) -> Result<()>;
}
