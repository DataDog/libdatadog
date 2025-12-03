// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! CXX bindings for crash_info module - provides a safe and idiomatic C++ API

use super::builder::CrashInfoBuilder;
use super::stacktrace::{StackFrame, StackTrace};
use super::CrashInfo;
use crate::{BuildIdType, FileType, Metadata};

// ============================================================================
// CXX Bridge - C++ Bindings
// ============================================================================

#[cxx::bridge(namespace = "datadog::crashtracker")]
pub mod ffi {
    // Shared enums
    #[repr(u32)]
    enum ErrorKind {
        Panic = 0,
        UnhandledException = 1,
        UnixSignal = 2,
    }

    enum BuildIdType {
        GNU,
        GO,
        PDB,
        SHA1,
    }

    enum FileType {
        APK,
        ELF,
        PE,
    }

    // Shared structs
    struct Metadata {
        library_name: String,
        library_version: String,
        family: String,
        tags: Vec<String>,
    }

    struct ProcInfo {
        pid: u32,
    }

    struct OsInfo {
        architecture: String,
        bitness: String,
        os_type: String,
        version: String,
    }

    // Opaque Rust types
    extern "Rust" {
        type CrashInfoBuilder;
        type StackFrame;
        type StackTrace;
        type CrashInfo;

        // Static factory methods
        #[Self = "CrashInfoBuilder"]
        fn create() -> Box<CrashInfoBuilder>;

        #[Self = "StackFrame"]
        fn create() -> Box<StackFrame>;

        #[Self = "StackTrace"]
        fn create() -> Box<StackTrace>;

        // CrashInfoBuilder methods - need wrappers for type conversion
        fn set_kind(self: &mut CrashInfoBuilder, kind: ErrorKind) -> Result<()>;
        fn set_metadata(self: &mut CrashInfoBuilder, metadata: Metadata) -> Result<()>;
        fn set_proc_info(self: &mut CrashInfoBuilder, proc_info: ProcInfo) -> Result<()>;
        fn set_os_info(self: &mut CrashInfoBuilder, os_info: OsInfo) -> Result<()>;
        fn add_stack(self: &mut CrashInfoBuilder, stack: Box<StackTrace>) -> Result<()>;
        fn add_stack_frame(
            self: &mut CrashInfoBuilder,
            frame: Box<StackFrame>,
            incomplete: bool,
        ) -> Result<()>;

        // CrashInfoBuilder methods - exposed directly (no conversion needed)
        fn with_message(self: &mut CrashInfoBuilder, message: String) -> Result<()>;
        fn with_fingerprint(self: &mut CrashInfoBuilder, fingerprint: String) -> Result<()>;
        fn with_incomplete(self: &mut CrashInfoBuilder, incomplete: bool) -> Result<()>;
        fn with_counter(self: &mut CrashInfoBuilder, name: String, value: i64) -> Result<()>;
        fn with_log_message(
            self: &mut CrashInfoBuilder,
            message: String,
            also_print: bool,
        ) -> Result<()>;
        fn with_file(self: &mut CrashInfoBuilder, filename: String) -> Result<()>;
        fn with_timestamp_now(self: &mut CrashInfoBuilder) -> Result<()>;
        fn with_os_info_this_machine(self: &mut CrashInfoBuilder) -> Result<()>;
        fn with_stack_set_complete(self: &mut CrashInfoBuilder) -> Result<()>;

        // Build function
        fn crashinfo_build(builder: Box<CrashInfoBuilder>) -> Result<Box<CrashInfo>>;

        // StackFrame methods - wrappers for FFI type conversion
        fn build_id_type(self: &mut StackFrame, build_id_type: BuildIdType);
        fn file_type(self: &mut StackFrame, file_type: FileType);

        // StackFrame methods - exposed directly
        fn with_ip(self: &mut StackFrame, ip: usize);
        fn with_module_base_address(self: &mut StackFrame, addr: usize);
        fn with_sp(self: &mut StackFrame, sp: usize);
        fn with_symbol_address(self: &mut StackFrame, addr: usize);
        fn with_build_id(self: &mut StackFrame, build_id: String);
        fn with_path(self: &mut StackFrame, path: String);
        fn with_relative_address(self: &mut StackFrame, addr: usize);
        fn with_function(self: &mut StackFrame, function: String);
        fn with_file(self: &mut StackFrame, file: String);
        fn with_line(self: &mut StackFrame, line: u32);
        fn with_column(self: &mut StackFrame, column: u32);

        // StackTrace methods
        fn add_frame(self: &mut StackTrace, frame: Box<StackFrame>, incomplete: bool)
            -> Result<()>;
        #[cxx_name = "mark_complete"]
        fn set_complete(self: &mut StackTrace) -> Result<()>;

        // CrashInfo methods
        fn to_json(self: &CrashInfo) -> Result<String>;
    }
}

// ============================================================================
// Static Factory Methods
// ============================================================================

impl CrashInfoBuilder {
    pub fn create() -> Box<CrashInfoBuilder> {
        Box::new(CrashInfoBuilder::default())
    }
}

impl StackFrame {
    pub fn create() -> Box<StackFrame> {
        Box::new(StackFrame::new())
    }
}

impl StackTrace {
    pub fn create() -> Box<StackTrace> {
        Box::new(StackTrace::new_incomplete())
    }
}

// ============================================================================
// CrashInfoBuilder - Type Conversion Wrappers
// ============================================================================

impl CrashInfoBuilder {
    pub fn set_kind(&mut self, kind: ffi::ErrorKind) -> anyhow::Result<()> {
        let internal_kind = match kind {
            ffi::ErrorKind::Panic => crate::ErrorKind::Panic,
            ffi::ErrorKind::UnhandledException => crate::ErrorKind::UnhandledException,
            ffi::ErrorKind::UnixSignal => crate::ErrorKind::UnixSignal,
            _ => anyhow::bail!("Unknown error kind"),
        };
        self.with_kind(internal_kind)
    }

    pub fn set_metadata(&mut self, metadata: ffi::Metadata) -> anyhow::Result<()> {
        let internal_metadata = Metadata::new(
            metadata.library_name,
            metadata.library_version,
            metadata.family,
            metadata.tags,
        );
        self.with_metadata(internal_metadata)
    }

    pub fn set_proc_info(&mut self, proc_info: ffi::ProcInfo) -> anyhow::Result<()> {
        let internal_proc_info = crate::ProcInfo { pid: proc_info.pid };
        self.with_proc_info(internal_proc_info)
    }

    pub fn set_os_info(&mut self, os_info: ffi::OsInfo) -> anyhow::Result<()> {
        let internal_os_info = crate::OsInfo {
            architecture: os_info.architecture,
            bitness: os_info.bitness,
            os_type: os_info.os_type,
            version: os_info.version,
        };
        self.with_os_info(internal_os_info)
    }

    #[allow(clippy::boxed_local)]
    pub fn add_stack(&mut self, stack: Box<StackTrace>) -> anyhow::Result<()> {
        self.with_stack(*stack)
    }

    #[allow(clippy::boxed_local)]
    pub fn add_stack_frame(
        &mut self,
        frame: Box<StackFrame>,
        incomplete: bool,
    ) -> anyhow::Result<()> {
        self.with_stack_frame(*frame, incomplete)
    }
}

pub fn crashinfo_build(builder: Box<CrashInfoBuilder>) -> anyhow::Result<Box<CrashInfo>> {
    Ok(Box::new(builder.build()?))
}

// ============================================================================
// StackFrame - Type Conversion Wrappers
// ============================================================================

impl StackFrame {
    pub fn build_id_type(&mut self, build_id_type: ffi::BuildIdType) {
        let internal_type = match build_id_type {
            ffi::BuildIdType::GNU => BuildIdType::GNU,
            ffi::BuildIdType::GO => BuildIdType::GO,
            ffi::BuildIdType::PDB => BuildIdType::PDB,
            ffi::BuildIdType::SHA1 => BuildIdType::SHA1,
            _ => return,
        };
        self.set_build_id_type(internal_type);
    }

    pub fn file_type(&mut self, file_type: ffi::FileType) {
        let internal_type = match file_type {
            ffi::FileType::APK => FileType::APK,
            ffi::FileType::ELF => FileType::ELF,
            ffi::FileType::PE => FileType::PE,
            _ => return,
        };
        self.set_file_type(internal_type);
    }
}

// ============================================================================
// StackTrace - Wrapper Methods
// ============================================================================

impl StackTrace {
    // Wrapper to unbox the frame parameter since CXX requires Box<T> for opaque types
    #[allow(clippy::boxed_local)]
    pub fn add_frame(&mut self, frame: Box<StackFrame>, incomplete: bool) -> anyhow::Result<()> {
        StackTrace::push_frame(self, *frame, incomplete)
    }
}

// ============================================================================
// CrashInfo - Utility Functions
// ============================================================================

impl CrashInfo {
    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}
