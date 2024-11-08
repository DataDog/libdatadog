// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct StackFrameNames {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub colno: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub lineno: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub name: Option<String>,
}

/// All fields are hex encoded integers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackFrame {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub module_base_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub names: Option<Vec<StackFrameNames>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub normalized_ip: Option<NormalizedAddress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub sp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub symbol_address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NormalizedAddressMeta {
    Apk(PathBuf),
    Elf {
        path: PathBuf,
        build_id: Option<Vec<u8>>,
    },
    Pdb {
        path: PathBuf,
        guid: Option<Vec<u8>>,
        age: u64,
    },
    Unknown,
    Unexpected(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedAddress {
    pub file_offset: u64,
    pub meta: NormalizedAddressMeta,
}

#[cfg(unix)]
mod unix {
    use super::*;
    use anyhow::anyhow;
    use blazesym::{
        helper::ElfResolver,
        normalize::{Normalizer, UserMeta},
        symbolize::{Input, Source, Sym, Symbolized, Symbolizer, TranslateFileOffset},
        Pid,
    };

    impl<'src> From<&UserMeta<'src>> for NormalizedAddressMeta {
        fn from(value: &UserMeta<'src>) -> Self {
            match value {
                UserMeta::Apk(a) => Self::Apk(a.path.clone()),
                UserMeta::Elf(e) => Self::Elf {
                    path: e.path.clone(),
                    build_id: e.build_id.as_ref().map(|cow| cow.clone().into_owned()),
                },
                UserMeta::Unknown(_) => Self::Unknown,
                _ => Self::Unexpected(format!("{value:?}")),
            }
        }
    }

    impl From<Sym<'_>> for StackFrameNames {
        fn from(value: Sym) -> Self {
            let mut rval = Self::default();
            if let Some(c) = value.code_info {
                rval.lineno = c.line;
                rval.filename = Some(c.to_path().display().to_string());
                rval.colno = c.column.map(|c| c.into());
            }
            rval.name = Some(value.name.into_owned());
            rval
        }
    }

    impl StackFrame {
        pub fn normalize_ip(&mut self, normalizer: &Normalizer, pid: Pid) -> anyhow::Result<()> {
            if let Some(ip) = &self.ip {
                let ip = ip.trim_start_matches("0x");
                let ip = u64::from_str_radix(ip, 16)?;
                let normed = normalizer.normalize_user_addrs(pid, &[ip])?;
                anyhow::ensure!(normed.outputs.len() == 1);
                let (file_offset, meta_idx) = normed.outputs[0];
                let meta = &normed.meta[meta_idx];
                let elf = meta.as_elf().ok_or(anyhow::anyhow!("Not elf"))?;
                let resolver = ElfResolver::open(&elf.path)?;
                let virt_address = resolver
                    .file_offset_to_virt_offset(file_offset)?
                    .ok_or(anyhow!("No matching segment found"))?;
                self.normalized_ip = Some(NormalizedAddress {
                    file_offset: virt_address,
                    meta: meta.into(),
                });
            }
            Ok(())
        }

        pub fn resolve_names(
            &mut self,
            src: &Source,
            symbolizer: &Symbolizer,
        ) -> anyhow::Result<()> {
            if let Some(ip) = &self.ip {
                let ip = ip.trim_start_matches("0x");
                let ip = u64::from_str_radix(ip, 16)?;
                let input = Input::AbsAddr(ip);
                match symbolizer.symbolize_single(src, input)? {
                    Symbolized::Sym(s) => {
                        //TODO: handle
                        self.names = Some(vec![s.into()]);
                    }
                    Symbolized::Unknown(reason) => {
                        anyhow::bail!("Couldn't symbolize {ip}: {reason}");
                    }
                }
            }
            Ok(())
        }
    }
}
