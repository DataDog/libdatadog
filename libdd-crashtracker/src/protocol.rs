// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

macro_rules! protocol_marker {
    ($vis:vis const $name:ident: &str = $value:literal;) => {
        #[allow(dead_code)]
        $vis const $name: &str = $value;
    };
}

pub const DD_CRASHTRACK_BEGIN_ADDITIONAL_TAGS: &str = "DD_CRASHTRACK_BEGIN_ADDITIONAL_TAGS";
pub const DD_CRASHTRACK_BEGIN_CONFIG: &str = "DD_CRASHTRACK_BEGIN_CONFIG";
protocol_marker!(
    pub const DD_CRASHTRACK_BEGIN_WHOLE_STACKTRACE: &str = "DD_CRASHTRACK_BEGIN_WHOLE_STACKTRACE";
);
protocol_marker!(
    pub const DD_CRASHTRACK_BEGIN_COUNTERS: &str = "DD_CRASHTRACK_BEGIN_COUNTERS";
);
protocol_marker!(
    pub const DD_CRASHTRACK_BEGIN_FILE: &str = "DD_CRASHTRACK_BEGIN_FILE";
);
pub const DD_CRASHTRACK_BEGIN_KIND: &str = "DD_CRASHTRACK_BEGIN_KIND";
pub const DD_CRASHTRACK_BEGIN_METADATA: &str = "DD_CRASHTRACK_BEGIN_METADATA";
pub const DD_CRASHTRACK_BEGIN_PROCINFO: &str = "DD_CRASHTRACK_BEGIN_PROCESSINFO";
protocol_marker!(
    pub const DD_CRASHTRACK_BEGIN_RUNTIME_STACK_FRAME: &str =
        "DD_CRASHTRACK_BEGIN_RUNTIME_STACK_FRAME";
);
protocol_marker!(
    pub const DD_CRASHTRACK_BEGIN_RUNTIME_STACK_STRING: &str =
        "DD_CRASHTRACK_BEGIN_RUNTIME_STACK_STRING";
);
pub const DD_CRASHTRACK_BEGIN_SIGINFO: &str = "DD_CRASHTRACK_BEGIN_SIGINFO";
protocol_marker!(
    pub const DD_CRASHTRACK_BEGIN_SPAN_IDS: &str = "DD_CRASHTRACK_BEGIN_SPAN_IDS";
);
pub const DD_CRASHTRACK_BEGIN_STACKTRACE: &str = "DD_CRASHTRACK_BEGIN_STACKTRACE";
protocol_marker!(
    pub const DD_CRASHTRACK_BEGIN_TRACE_IDS: &str = "DD_CRASHTRACK_BEGIN_TRACE_IDS";
);
protocol_marker!(
    pub const DD_CRASHTRACK_BEGIN_UCONTEXT: &str = "DD_CRASHTRACK_BEGIN_UCONTEXT";
);
pub const DD_CRASHTRACK_BEGIN_MESSAGE: &str = "DD_CRASHTRACK_BEGIN_MESSAGE";
pub const DD_CRASHTRACK_DONE: &str = "DD_CRASHTRACK_DONE";
pub const DD_CRASHTRACK_END_ADDITIONAL_TAGS: &str = "DD_CRASHTRACK_END_ADDITIONAL_TAGS";
pub const DD_CRASHTRACK_END_CONFIG: &str = "DD_CRASHTRACK_END_CONFIG";
protocol_marker!(
    pub const DD_CRASHTRACK_END_WHOLE_STACKTRACE: &str = "DD_CRASHTRACK_END_WHOLE_STACKTRACE";
);
protocol_marker!(
    pub const DD_CRASHTRACK_END_COUNTERS: &str = "DD_CRASHTRACK_END_COUNTERS";
);
protocol_marker!(
    pub const DD_CRASHTRACK_END_FILE: &str = "DD_CRASHTRACK_END_FILE";
);
pub const DD_CRASHTRACK_END_KIND: &str = "DD_CRASHTRACK_END_KIND";
pub const DD_CRASHTRACK_END_METADATA: &str = "DD_CRASHTRACK_END_METADATA";
pub const DD_CRASHTRACK_END_PROCINFO: &str = "DD_CRASHTRACK_END_PROCESSINFO";
protocol_marker!(
    pub const DD_CRASHTRACK_END_RUNTIME_STACK_FRAME: &str = "DD_CRASHTRACK_END_RUNTIME_STACK_FRAME";
);
protocol_marker!(
    pub const DD_CRASHTRACK_END_RUNTIME_STACK_STRING: &str =
        "DD_CRASHTRACK_END_RUNTIME_STACK_STRING";
);
pub const DD_CRASHTRACK_END_SIGINFO: &str = "DD_CRASHTRACK_END_SIGINFO";
protocol_marker!(
    pub const DD_CRASHTRACK_END_SPAN_IDS: &str = "DD_CRASHTRACK_END_SPAN_IDS";
);
pub const DD_CRASHTRACK_END_STACKTRACE: &str = "DD_CRASHTRACK_END_STACKTRACE";
protocol_marker!(
    pub const DD_CRASHTRACK_END_TRACE_IDS: &str = "DD_CRASHTRACK_END_TRACE_IDS";
);
protocol_marker!(
    pub const DD_CRASHTRACK_END_UCONTEXT: &str = "DD_CRASHTRACK_END_UCONTEXT";
);
pub const DD_CRASHTRACK_END_MESSAGE: &str = "DD_CRASHTRACK_END_MESSAGE";

pub trait ByteSink {
    type Error;

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
}

#[cfg(feature = "std")]
impl<W: std::io::Write + ?Sized> ByteSink for W {
    type Error = std::io::Error;

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.write_all(bytes)
    }
}

pub fn marker_line<S, E>(sink: &mut S, marker: &str) -> Result<(), E>
where
    S: ByteSink + ?Sized,
    E: From<S::Error>,
{
    sink.write_bytes(marker.as_bytes()).map_err(E::from)?;
    sink.write_bytes(b"\n").map_err(E::from)
}

pub fn section<S, E>(
    sink: &mut S,
    begin: &str,
    end: &str,
    body: impl FnOnce(&mut S) -> Result<(), E>,
) -> Result<(), E>
where
    S: ByteSink + ?Sized,
    E: From<S::Error>,
{
    marker_line(sink, begin)?;
    body(sink)?;
    marker_line(sink, end)
}
