// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(unix)]
use blazesym::{
    helper::ElfResolver,
    normalize::Normalizer,
    symbolize::{Input, Source, Symbolized, Symbolizer, TranslateFileOffset},
    Pid,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use symbolic_demangle::demangle;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StackTrace {
    pub format: String,
    pub frames: Vec<StackFrame>,
    pub incomplete: bool,
}

const FORMAT_STRING: &str = "Datadog Crashtracker 1.0";

impl StackTrace {
    pub fn empty() -> Self {
        Self {
            format: FORMAT_STRING.to_string(),
            frames: vec![],
            incomplete: false,
        }
    }

    pub fn from_frames(frames: Vec<StackFrame>, incomplete: bool) -> Self {
        Self {
            format: FORMAT_STRING.to_string(),
            frames,
            incomplete,
        }
    }

    pub fn new_incomplete() -> Self {
        Self {
            format: FORMAT_STRING.to_string(),
            frames: vec![],
            incomplete: true,
        }
    }

    pub fn missing() -> Self {
        Self {
            format: FORMAT_STRING.to_string(),
            frames: vec![],
            incomplete: true,
        }
    }
}

impl StackTrace {
    pub fn set_complete(&mut self) -> anyhow::Result<()> {
        self.incomplete = false;
        Ok(())
    }

    pub fn push_frame(&mut self, frame: StackFrame, incomplete: bool) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.incomplete,
            "Can't push a new frame onto a complete stack"
        );
        self.frames.push(frame);
        self.incomplete = incomplete;
        Ok(())
    }
}

#[cfg(unix)]
impl StackTrace {
    pub fn normalize_ips(&mut self, normalizer: &Normalizer, pid: Pid) -> anyhow::Result<()> {
        let mut errors = 0;
        for frame in &mut self.frames {
            frame.normalize_ip(normalizer, pid).unwrap_or_else(|e| {
                frame.comments.push(e.to_string());
                errors += 1;
            });
        }
        anyhow::ensure!(errors == 0);
        Ok(())
    }

    pub fn resolve_names(&mut self, src: &Source, symbolizer: &Symbolizer) -> anyhow::Result<()> {
        let mut errors = 0;
        for frame in &mut self.frames {
            frame.resolve_names(src, symbolizer).unwrap_or_else(|e| {
                frame.comments.push(e.to_string());
                errors += 1;
            });
        }
        anyhow::ensure!(errors == 0);
        Ok(())
    }
}

impl Default for StackTrace {
    fn default() -> Self {
        Self::missing()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct StackFrame {
    // Absolute addresses
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_base_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_address: Option<String>,

    // Relative addresses
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id_type: Option<BuildIdType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_type: Option<FileType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_address: Option<String>,

    // Debug Info
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mangled_name: Option<String>,

    // Additional Info
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<String>,
}

impl StackFrame {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(unix)]
impl StackFrame {
    pub fn normalize_ip(&mut self, normalizer: &Normalizer, pid: Pid) -> anyhow::Result<()> {
        use anyhow::Context;
        if let Some(ip) = &self.ip {
            let ip = ip.trim_start_matches("0x");
            let ip = u64::from_str_radix(ip, 16)?;
            let normed = normalizer.normalize_user_addrs(pid, &[ip])?;
            anyhow::ensure!(normed.outputs.len() == 1);
            let (file_offset, meta_idx) = normed.outputs[0];
            let meta = &normed.meta[meta_idx];
            let elf = meta.as_elf().context("Not elf")?;
            let resolver = ElfResolver::open(&elf.path)?;
            let virt_address = resolver
                .file_offset_to_virt_offset(file_offset)?
                .context("No matching segment found")?;

            self.build_id = elf.build_id.as_ref().map(|x| byte_slice_as_hex(x.as_ref()));
            self.build_id_type = Some(BuildIdType::GNU);
            self.file_type = Some(FileType::ELF);
            self.path = Some(elf.path.to_string_lossy().to_string());
            self.relative_address = Some(format!("{virt_address:#018x}"));
        }
        Ok(())
    }

    pub fn resolve_names(&mut self, src: &Source, symbolizer: &Symbolizer) -> anyhow::Result<()> {
        if let Some(ip) = &self.ip {
            let ip = ip.trim_start_matches("0x");
            let ip = u64::from_str_radix(ip, 16)?;
            let input = Input::AbsAddr(ip);
            match symbolizer.symbolize_single(src, input)? {
                Symbolized::Sym(s) => {
                    if let Some(c) = s.code_info {
                        self.column = c.column.map(u32::from);
                        self.file = Some(c.to_path().display().to_string());
                        self.line = c.line;
                    }
                    self.function = Some(s.name.into_owned());
                }
                Symbolized::Unknown(reason) => {
                    anyhow::bail!("Couldn't symbolize {ip}: {reason}");
                }
            }
        }
        Ok(())
    }

    pub fn demangle_name(&mut self) -> anyhow::Result<()> {
        if let Some(name) = &self.function {
            let demangled = demangle(name);
            if demangled != *name {
                self.mangled_name = Some(name.clone());
                self.function = Some(demangled.to_string());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[allow(clippy::upper_case_acronyms)]
#[repr(C)]
pub enum BuildIdType {
    GNU,
    GO,
    PDB,
    SHA1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[allow(clippy::upper_case_acronyms)]
#[repr(C)]
pub enum FileType {
    APK,
    ELF,
    PE,
}

#[cfg(unix)]
fn byte_slice_as_hex(bv: &[u8]) -> String {
    use std::fmt::Write;

    let mut s = String::new();
    for byte in bv {
        let _ = write!(&mut s, "{byte:X}");
    }
    s
}

#[cfg(test)]
impl super::test_utils::TestInstance for StackTrace {
    fn test_instance(_seed: u64) -> Self {
        let frames = (0..10).map(StackFrame::test_instance).collect();
        Self::from_frames(frames, false)
    }
}

#[cfg(test)]
impl super::test_utils::TestInstance for StackFrame {
    fn test_instance(seed: u64) -> Self {
        let ip = Some(format!("{seed:#x}"));
        let module_base_address = None;
        let sp = None;
        let symbol_address = None;

        let build_id = Some(format!("abcde{seed:#x}"));
        let build_id_type = Some(BuildIdType::GNU);
        let file_type = Some(FileType::ELF);
        let path = Some(format!("/usr/bin/foo{seed}"));
        let relative_address = None;

        let column = Some(2 * seed as u32);
        let file = Some(format!("banana{seed}.rs"));
        let function = Some(format!("Bar::baz{seed}"));
        let mangled_name = Some(format!("_ZN3Bar3baz{seed}E"));
        let line = Some((2 * seed + 1) as u32);

        let comments = vec![format!("This is a comment on frame {seed}")];
        Self {
            ip,
            module_base_address,
            sp,
            symbol_address,
            build_id,
            build_id_type,
            file_type,
            path,
            relative_address,
            column,
            file,
            function,
            mangled_name,
            line,
            comments,
        }
    }
}
