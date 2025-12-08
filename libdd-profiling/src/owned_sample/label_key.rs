// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Well-known label keys used in profiling.
///
/// These correspond to standard labels that profilers commonly attach to samples,
/// such as thread information, exception details, and tracing context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LabelKey {
    ExceptionType,
    ThreadId,
    ThreadNativeId,
    ThreadName,
    TaskId,
    TaskName,
    SpanId,
    LocalRootSpanId,
    TraceType,
    ClassName,
    LockName,
    GpuDeviceName,
}

impl LabelKey {
    /// Returns the string representation of this label key.
    ///
    /// # Example
    /// ```
    /// # use libdd_profiling::owned_sample::LabelKey;
    /// assert_eq!(LabelKey::ThreadId.as_str(), "thread id");
    /// assert_eq!(LabelKey::ExceptionType.as_str(), "exception type");
    /// ```
    pub const fn as_str(self) -> &'static str {
        match self {
            LabelKey::ExceptionType => "exception type",
            LabelKey::ThreadId => "thread id",
            LabelKey::ThreadNativeId => "thread native id",
            LabelKey::ThreadName => "thread name",
            LabelKey::TaskId => "task id",
            LabelKey::TaskName => "task name",
            LabelKey::SpanId => "span id",
            LabelKey::LocalRootSpanId => "local root span id",
            LabelKey::TraceType => "trace type",
            LabelKey::ClassName => "class name",
            LabelKey::LockName => "lock name",
            LabelKey::GpuDeviceName => "gpu device name",
        }
    }
}

impl AsRef<str> for LabelKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for LabelKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
