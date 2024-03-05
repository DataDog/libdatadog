// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use blazesym::symbolize::{Input, Source, Sym, Symbolized, Symbolizer};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct StackFrameNames {
    pub colno: Option<u32>,
    pub filename: Option<String>,
    pub lineno: Option<u32>,
    pub name: Option<String>,
}

/// All fields are hex encoded integers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackFrame {
    pub ip: Option<String>,
    pub module_base_address: Option<String>,
    pub names: Option<Vec<StackFrameNames>>,
    pub sp: Option<String>,
    pub symbol_address: Option<String>,
}

impl StackFrame {
    pub fn resolve_names(&mut self, src: &Source, symbolizer: &Symbolizer) -> anyhow::Result<()> {
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
