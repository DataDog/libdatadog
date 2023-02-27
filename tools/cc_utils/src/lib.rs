// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{
    env,
    ffi::{self, OsString},
    path::{Path, PathBuf},
};

// reexport cc
pub use cc;

#[derive(Clone, Debug)]
pub enum LinkableTarget {
    Path(PathBuf),
    Name(String),
}

#[derive(Clone, Debug)]
pub enum Linkable {
    Static(LinkableTarget),
    Dynamic(LinkableTarget),
}

#[derive(Clone, Debug)]
enum OutputType {
    Executable,
    Shared,
}

#[derive(Clone, Debug)]
pub struct ImprovedBuild {
    files: Vec<PathBuf>,
    linkables: Vec<Linkable>,
    cc_build: cc::Build,
    emit_rerun_if_env_changed: bool,
}

impl Linkable {
    fn concat_os_strings(a: &ffi::OsStr, b: &ffi::OsStr) -> OsString {
        let mut ret = OsString::with_capacity(a.len() + b.len());
        ret.push(a);
        ret.push(b);
        ret
    }

    pub fn to_compiler_args(&self, _compiler: &cc::Tool) -> Vec<OsString> {
        // todo: improve handling of static and dynamic link cases

        match self {
            Linkable::Static(target) => match target {
                LinkableTarget::Path(p) => vec![p.as_os_str().to_owned()],
                LinkableTarget::Name(name) => vec![format!("-l{name}").into()],
            },
            Linkable::Dynamic(target) => match target {
                LinkableTarget::Path(path) => {
                    let mut args = vec![];

                    if let Some(dirname) = path.parent() {
                        args.push(Self::concat_os_strings(
                            ffi::OsStr::new("-L"),
                            dirname.as_os_str(),
                        ))
                    }
                    if let Some(filename) = path.file_name() {
                        args.push(Self::concat_os_strings(ffi::OsStr::new("-l"), filename))
                    }
                    args
                }
                LinkableTarget::Name(name) => vec![format!("-l{name}").into()],
            },
        }
    }
}

impl ImprovedBuild {
    pub fn file<P: AsRef<Path>>(&mut self, p: P) -> &mut Self {
        self.files.push(p.as_ref().to_path_buf());
        self
    }

    pub fn files<P>(&mut self, p: P) -> &mut Self
    where
        P: IntoIterator,
        P::Item: AsRef<Path>,
    {
        for file in p.into_iter() {
            self.file(file);
        }
        self
    }

    pub fn link_dynamically<S: AsRef<str>>(&mut self, name: S) -> &mut Self {
        self.linkables.push(Linkable::Dynamic(LinkableTarget::Name(
            name.as_ref().to_owned(),
        )));
        self
    }

    pub fn set_cc_builder(&mut self, cc_build: cc::Build) -> &mut Self {
        self.cc_build = cc_build;
        self
    }

    pub fn emit_rerun_if_env_changed(&mut self, emit: bool) -> &mut Self {
        self.emit_rerun_if_env_changed = emit;
        self
    }

    pub fn new() -> Self {
        let cc_build = cc::Build::new();

        ImprovedBuild {
            files: Default::default(),
            linkables: Default::default(),
            cc_build,
            emit_rerun_if_env_changed: false,
        }
    }

    fn get_out_dir(&self) -> anyhow::Result<PathBuf> {
        env::var_os("OUT_DIR")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::Error::msg("can't get output directory info"))
    }

    // cc::Build shadow
    pub fn define<'a, V: Into<Option<&'a str>>>(&mut self, var: &str, val: V) -> &mut Self {
        self.cc_build.define(var, val);
        self
    }

    pub fn warnings(&mut self, warnings: bool) -> &mut Self {
        self.cc_build.warnings(warnings);
        self
    }

    pub fn warnings_into_errors(&mut self, warnings_into_errors: bool) -> &mut Self {
        self.cc_build.warnings_into_errors(warnings_into_errors);
        self
    }

    pub fn include<P: AsRef<Path>>(&mut self, dir: P) -> &mut Self {
        self.cc_build.include(dir);
        self
    }

    pub fn includes<P>(&mut self, dirs: P) -> &mut Self
    where
        P: IntoIterator,
        P::Item: AsRef<Path>,
    {
        self.cc_build.includes(dirs);
        self
    }

    pub fn try_compile_executable(&self, output: &str) -> anyhow::Result<()> {
        self.try_compile_any(output, OutputType::Executable)
    }

    pub fn try_compile_shared_lib(&self, output: &str) -> anyhow::Result<()> {
        self.try_compile_any(output, OutputType::Shared)
    }

    fn try_compile_any(&self, output: &str, output_type: OutputType) -> anyhow::Result<()> {
        if self.emit_rerun_if_env_changed {
            for file in self.files.iter() {
                println!(
                    "cargo:rerun-if-changed={}",
                    file.as_path().to_string_lossy()
                );
            }
        }

        let compiler = self.cc_build.try_get_compiler()?;
        let output_path = self.get_out_dir()?.join(output);

        let mut cmd = compiler.to_command();

        match output_type {
            OutputType::Executable => {
                cmd.args(["-o".into(), output_path.as_os_str().to_owned()]);
            }
            OutputType::Shared => {
                cmd.args([
                    "-shared".into(),
                    "-o".into(),
                    output_path.as_os_str().to_owned(),
                ]);
            }
        }

        for file in &self.files {
            cmd.arg(file.as_os_str());
        }

        for linkable in &self.linkables {
            cmd.args(linkable.to_compiler_args(&compiler));
        }
        println!("compiling: {cmd:?}");
        let status = cmd.spawn()?.wait()?;

        if !status.success() {
            return Err(anyhow::format_err!("compilation failed"));
        }

        Ok(())
    }
}

impl Default for ImprovedBuild {
    fn default() -> Self {
        Self::new()
    }
}
