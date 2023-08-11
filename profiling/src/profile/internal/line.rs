// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::profile::pprof;
use crate::profile::FunctionId;
use std::fmt::Debug;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Line {
    /// The id of the corresponding Function for this line.
    pub function_id: FunctionId,
    /// Line number in source code.
    pub line: u32,
}

impl Line {
    pub fn new<T>(function_id: FunctionId, line: T) -> Self
    where
        T: TryInto<u32>,
        T::Error: Debug,
    {
        let line: u32 = line.try_into().expect("line number to fit into a u32");
        Self { function_id, line }
    }
}

impl From<Line> for pprof::Line {
    fn from(t: Line) -> Self {
        Self::from(&t)
    }
}

impl From<&Line> for pprof::Line {
    fn from(t: &Line) -> Self {
        Self {
            function_id: t.function_id.into(),
            line: t.line.into(),
        }
    }
}
