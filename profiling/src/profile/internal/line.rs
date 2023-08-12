// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::profile::{pprof, FunctionId, Id};
use std::fmt::Debug;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Line {
    /// The id of the corresponding Function for this line.
    pub function_id: FunctionId,
    /// Line number in source code.
    pub line: i64,
}

impl From<Line> for pprof::Line {
    fn from(t: Line) -> Self {
        Self::from(&t)
    }
}

impl From<&Line> for pprof::Line {
    fn from(t: &Line) -> Self {
        Self {
            function_id: t.function_id.to_raw_id(),
            line: t.line,
        }
    }
}
