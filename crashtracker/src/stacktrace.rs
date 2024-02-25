// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2024-Present Datadog, Inc.

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
    colno: Option<u32>,
    filename: Option<String>,
    lineno: Option<u32>,
    name: Option<String>,
}

/// All fields are hex encoded integers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackFrame {
    ip: Option<String>,
    module_base_address: Option<String>,
    names: Option<Vec<StackFrameNames>>,
    sp: Option<String>,
    symbol_address: Option<String>,
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
