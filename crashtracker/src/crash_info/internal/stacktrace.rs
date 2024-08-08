// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct StackFrameNames {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colno: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineno: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// All fields are hex encoded integers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_base_address: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<StackFrameNames>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_ip: Option<NormalizedAddress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NormalizedAddressMeta {
    Apk(PathBuf),
    Elf {
        path: PathBuf,
        build_id: Option<Vec<u8>>,
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
    use blazesym::{
        normalize::{Normalizer, UserMeta},
        symbolize::{Input, Source, Sym, Symbolized, Symbolizer},
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
                let meta = (&normed.meta[meta_idx]).into();
                self.normalized_ip = Some(NormalizedAddress { file_offset, meta });
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
                        self.names = vec![s.into()];
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
