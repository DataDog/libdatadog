// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::NormalizedAddress;
use anyhow::Context;
#[cfg(unix)]
use blazesym::{
    helper::ElfResolver,
    normalize::Normalizer,
    symbolize::{Input, Source, Symbolized, Symbolizer, TranslateFileOffset},
    Pid,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StackTrace {
    pub format: String,
    pub frames: Vec<StackFrame>,
}

impl StackTrace {
    pub fn new() -> Self {
        Self {
            format: "Datadog Crashtracker 1.0".to_string(),
            frames: vec![],
        }
    }

    pub fn normalize_ips(&mut self, normalizer: &Normalizer, pid: Pid) -> anyhow::Result<()> {
        for frame in &mut self.frames {
            // TODO: Should this keep going on failure, and report at the end?
            frame.normalize_ip(normalizer, pid)?;
        }
        Ok(())
    }

    pub fn resolve_names(&mut self, src: &Source, symbolizer: &Symbolizer) -> anyhow::Result<()> {
        for frame in &mut self.frames {
            // TODO: Should this keep going on failure, and report at the end?
            frame.resolve_names(src, symbolizer)?;
        }
        Ok(())
    }
}

impl Default for StackTrace {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Vec<crate::StackFrame>> for StackTrace {
    fn from(value: Vec<crate::StackFrame>) -> Self {
        #[allow(clippy::type_complexity)]
        fn convert_normalized_address(
            value: Option<NormalizedAddress>,
        ) -> (
            Option<String>,      // build_id
            Option<BuildIdType>, // build_id_type
            Option<FileType>,    // file_type
            Option<String>,      // path
            Option<String>,      // relative_address
        ) {
            if let Some(normalized_address) = value {
                let relative_address = Some(format!("{:#018x}", normalized_address.file_offset));
                let (build_id, build_id_type, file_type, path) = match normalized_address.meta {
                    crate::NormalizedAddressMeta::Apk(path_buf) => (
                        None,
                        None,
                        Some(FileType::APK),
                        Some(path_buf.to_string_lossy().to_string()),
                    ),
                    crate::NormalizedAddressMeta::Elf { path, build_id } => (
                        byte_vec_as_hex(build_id),
                        Some(BuildIdType::GNU),
                        Some(FileType::ELF),
                        Some(path.to_string_lossy().to_string()),
                    ),
                    crate::NormalizedAddressMeta::Pdb { path, guid, age } => (
                        Some(format!(
                            "{}{age}",
                            byte_vec_as_hex(guid).unwrap_or_default()
                        )),
                        Some(BuildIdType::PDB),
                        Some(FileType::PDB),
                        Some(path.to_string_lossy().to_string()),
                    ),
                    crate::NormalizedAddressMeta::Unknown => {
                        eprintln!("Unexpected NormalizedAddressMeta::Unknown");
                        (None, None, None, None)
                    }
                    crate::NormalizedAddressMeta::Unexpected(msg) => {
                        eprintln!("Unexpected NormalizedAddressMeta::Unexpected({msg})");
                        (None, None, None, None)
                    }
                };
                (build_id, build_id_type, file_type, relative_address, path)
            } else {
                (None, None, None, None, None)
            }
        }

        let format = String::from("Datadog Crashtracker 1.0");
        // Todo: this will under-estimate the cap needed if there are inlined functions.
        // Maybe not worth fixing this.
        let mut frames = Vec::with_capacity(value.len());
        for frame in value {
            let ip = frame.ip;
            let sp = frame.sp;
            let symbol_address = frame.symbol_address;
            let module_base_address = frame.module_base_address;

            let (build_id, build_id_type, file_type, relative_address, path) =
                convert_normalized_address(frame.normalized_ip);
            let base_frame = StackFrame {
                ip,
                sp,
                symbol_address,
                module_base_address,
                build_id,
                build_id_type,
                file_type,
                relative_address,
                path,
                column: None,
                file: None,
                line: None,
                function: None,
            };
            let names = frame.names.unwrap_or_default();
            if names.is_empty() {
                frames.push(base_frame);
            } else {
                for name in names {
                    frames.push(StackFrame {
                        column: name.colno,
                        file: name.filename,
                        function: name.name,
                        line: name.lineno,
                        ..base_frame.clone()
                    })
                }
            }
        }

        Self { format, frames }
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
}

impl StackFrame {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(unix)]
impl StackFrame {
    pub fn normalize_ip(&mut self, normalizer: &Normalizer, pid: Pid) -> anyhow::Result<()> {
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[allow(clippy::upper_case_acronyms)]
#[repr(C)]
pub enum BuildIdType {
    GNU,
    GO,
    PDB,
    PE,
    SHA1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[allow(clippy::upper_case_acronyms)]
#[repr(C)]
pub enum FileType {
    APK,
    ELF,
    PDB,
}

fn byte_vec_as_hex(bv: Option<Vec<u8>>) -> Option<String> {
    use std::fmt::Write;

    if let Some(bv) = bv {
        let mut s = String::new();
        for byte in bv {
            let _ = write!(&mut s, "{byte:X}");
        }
        Some(s)
    } else {
        None
    }
}

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
        Self {
            format: "Datadog Crashtracker 1.0".to_string(),
            frames,
        }
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
        let line = Some((2 * seed + 1) as u32);
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
            line,
        }
    }
}
