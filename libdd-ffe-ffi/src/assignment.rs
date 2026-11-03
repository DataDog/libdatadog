// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{CStr, c_char};
use std::pin::Pin;

use libdd_ffe::rules_based as ffe;
use libdd_ffe::rules_based::{
    Assignment, AssignmentReason, AssignmentValue, Configuration, EvaluationContext,
    EvaluationError, Str, now,
};

use crate::Handle;

/// Opaque type representing a result of evaluation.
pub struct ResolutionDetails {
    // Pin the inner result to ensure the memory address remains stable,
    // allowing us to safely store borrowed pointers to its contents.
    inner: Pin<Box<Result<Assignment, EvaluationError>>>,
    // memoizing some fields, so we can hand off references to them:
    error_message: Option<String>,
    flag_metadata: Vec<KeyValue<BorrowedStr, BorrowedStr>>,
    dependencies: Vec<ffe::EvaluationRecord>,
}
impl ResolutionDetails {
    fn new(value: Result<Assignment, EvaluationError>) -> ResolutionDetails {
        // Pin the value to the heap to ensure its memory address remains stable
        let inner = Box::pin(value);

        let error_message = (*inner).as_ref().err().map(|err| err.to_string());

        let mut flag_metadata = Vec::new();
        if let Ok(value) = (*inner).as_ref() {
            flag_metadata.push(KeyValue {
                key: BorrowedStr::borrow_static("__dd_allocation_key"),
                // SAFETY: The returned BorrowedStr does not outlive the source string v. The
                // source string v lives in the pinned Assignment inside `inner`, which is owned
                // by ResolutionDetails and will not move (guaranteed by Pin) or be modified (we
                // only hold shared references).
                value: unsafe { BorrowedStr::borrow_from_str(&value.allocation_key) },
            });
        }

        ResolutionDetails {
            inner,
            error_message,
            flag_metadata,
            dependencies: Vec::new(),
        }
    }
}
impl From<Assignment> for ResolutionDetails {
    fn from(value: Assignment) -> Self {
        ResolutionDetails::new(Ok(value))
    }
}
impl From<EvaluationError> for ResolutionDetails {
    fn from(value: EvaluationError) -> Self {
        ResolutionDetails::new(Err(value))
    }
}
impl From<Result<Assignment, EvaluationError>> for ResolutionDetails {
    fn from(value: Result<Assignment, EvaluationError>) -> Self {
        ResolutionDetails::new(value)
    }
}
impl AsRef<Result<Assignment, EvaluationError>> for ResolutionDetails {
    fn as_ref(&self) -> &Result<Assignment, EvaluationError> {
        &self.inner
    }
}

#[repr(C)]
pub enum ExpectedFlagType {
    String,
    Integer,
    Float,
    Boolean,
    Object,
    Number,
    Any,
}
impl From<ExpectedFlagType> for ffe::ExpectedFlagType {
    fn from(value: ExpectedFlagType) -> ffe::ExpectedFlagType {
        match value {
            ExpectedFlagType::String => ffe::ExpectedFlagType::String,
            ExpectedFlagType::Integer => ffe::ExpectedFlagType::Integer,
            ExpectedFlagType::Float => ffe::ExpectedFlagType::Float,
            ExpectedFlagType::Boolean => ffe::ExpectedFlagType::Boolean,
            ExpectedFlagType::Object => ffe::ExpectedFlagType::Object,
            ExpectedFlagType::Number => ffe::ExpectedFlagType::Number,
            ExpectedFlagType::Any => ffe::ExpectedFlagType::Any,
        }
    }
}

#[repr(C)]
pub enum FlagType {
    Unknown,
    String,
    Integer,
    Float,
    Boolean,
    Object,
}

impl From<ffe::FlagType> for FlagType {
    fn from(value: ffe::FlagType) -> Self {
        match value {
            ffe::FlagType::String => FlagType::String,
            ffe::FlagType::Integer => FlagType::Integer,
            ffe::FlagType::Float => FlagType::Float,
            ffe::FlagType::Boolean => FlagType::Boolean,
            ffe::FlagType::Object => FlagType::Object,
        }
    }
}

#[derive(Debug, PartialEq)]
#[repr(C)]
pub enum ErrorCode {
    Ok,
    TypeMismatch,
    ParseError,
    FlagNotFound,
    TargetingKeyMissing,
    InvalidContext,
    ProviderNotReady,
    General,
}

#[repr(C)]
pub enum Reason {
    Static,
    Default,
    TargetingMatch,
    Split,
    Disabled,
    Error,
}

/// Evaluates a feature flag.
///
/// # Ownership
///
/// The caller must call `ddog_ffe_assignment_drop` on the returned value to free resources.
///
/// # Safety
///
/// - `config` must be a valid `Configuration` handle
/// - `flag_key` must be a valid C string
/// - `context` must be a valid `EvaluationContext` handle
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn ddog_ffe_get_assignment(
    config: Handle<Configuration>,
    flag_key: *const c_char,
    expected_type: ExpectedFlagType,
    context: Handle<EvaluationContext>,
) -> Handle<ResolutionDetails> {
    catch_assignment_panic(|| {
        if flag_key.is_null() {
            return Handle::new(
                EvaluationError::Internal(Str::from_static_str(
                    "ddog_ffe_get_assignment: flag_key must not be NULL",
                ))
                .into(),
            );
        }

        // SAFETY: the caller must ensure that configuration handle is valid
        let config = unsafe { config.as_ref() };
        // SAFETY: the caller must ensure that context handle is valid
        let context = unsafe { context.as_ref() };

        // SAFETY: we checked that flag_key is not NULL.
        let Ok(flag_key) = unsafe { CStr::from_ptr(flag_key) }.to_str() else {
            return Handle::new(
                EvaluationError::Internal(Str::from_static_str(
                    "ddog_ffe_get_assignment: flag_key is not a valid UTF-8 string",
                ))
                .into(),
            );
        };

        let evaluation = config.eval_flag_with_trace(
            flag_key,
            context,
            expected_type.into(),
            now(),
            ffe::EvaluationOptions::default(),
        );
        let mut details = ResolutionDetails::new(evaluation.result);
        details.dependencies = evaluation.evaluations;
        Handle::new(details)
    })
}

fn catch_assignment_panic(
    evaluate: impl FnOnce() -> Handle<ResolutionDetails>,
) -> Handle<ResolutionDetails> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(evaluate)).unwrap_or_else(|_| {
        Handle::new(
            EvaluationError::Internal(Str::from_static_str("feature flag evaluation failed"))
                .into(),
        )
    })
}

/// Number of unique prerequisites reached by this evaluation.
/// # Safety
/// `assignment` must be a live handle.
#[unsafe(no_mangle)]
// allow(ffi-panic-boundary): reads the length of an owned vector.
pub unsafe extern "C" fn ddog_ffe_assignment_dependency_count(
    assignment: Handle<ResolutionDetails>,
) -> usize {
    // SAFETY: caller guarantees a live handle.
    unsafe { assignment.as_ref() }.dependencies.len()
}

/// Prerequisite key, borrowed until the root assignment is freed; empty for an invalid index.
/// # Safety
/// `assignment` must be a live handle.
#[unsafe(no_mangle)]
// allow(ffi-panic-boundary): bounds-checked lookup and borrowing do not allocate.
pub unsafe extern "C" fn ddog_ffe_assignment_dependency_key(
    assignment: Handle<ResolutionDetails>,
    index: usize,
) -> BorrowedStr {
    // SAFETY: caller guarantees a live handle.
    match unsafe { assignment.as_ref() }.dependencies.get(index) {
        // SAFETY: caller retains the root assignment throughout the borrow.
        Some(record) => unsafe { BorrowedStr::borrow_from_str(&record.flag_key) },
        None => BorrowedStr::empty(),
    }
}

/// Whether this prerequisite contributed to the root's successful assignment.
/// This does not override the prerequisite's `do_log` or SDK consent and deduplication.
/// # Safety
/// `assignment` must be a live handle.
#[unsafe(no_mangle)]
// allow(ffi-panic-boundary): bounds-checked lookup of a boolean does not allocate.
pub unsafe extern "C" fn ddog_ffe_assignment_dependency_exposure_eligible(
    assignment: Handle<ResolutionDetails>,
    index: usize,
) -> bool {
    // SAFETY: caller guarantees a live handle.
    unsafe { assignment.as_ref() }
        .dependencies
        .get(index)
        .is_some_and(|record| record.exposure_eligible)
}

/// Copies a prerequisite result into an independently owned handle, including errors.
/// Free it with `ddog_ffe_assignment_drop`. Invalid indices return a General error.
/// # Safety
/// `assignment` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_dependency_result(
    assignment: Handle<ResolutionDetails>,
    index: usize,
) -> Handle<ResolutionDetails> {
    catch_assignment_panic(|| {
        // SAFETY: caller guarantees a live handle.
        let result = unsafe { assignment.as_ref() }
            .dependencies
            .get(index)
            .map(|record| record.result.clone())
            .unwrap_or_else(|| {
                Err(EvaluationError::Internal(Str::from_static_str(
                    "invalid prerequisite index",
                )))
            });
        Handle::new(result.into())
    })
}

#[repr(C)]
pub enum VariantValue {
    /// Evaluation did not produce any value.
    None,
    String(BorrowedStr),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Object(BorrowedStr),
}

/// A string that has been borrowed. Beware that it is NOT nul-terminated!
///
/// # Ownership
///
/// This string is non-owning. You must not free `ptr`.
///
/// # Safety
///
/// - The string is not NUL-terminated, it can only be used with API that accept the len as an
///   additional parameter.
/// - The value must not be used after the value it borrowed from has been moved, modified, or
///   freed.
#[repr(C)]
pub struct BorrowedStr {
    /// May be NULL if `len` is `0`.
    pub ptr: *const u8,
    pub len: usize,
}
impl BorrowedStr {
    #[inline]
    pub(crate) unsafe fn as_bytes(&self) -> &[u8] {
        // SAFETY: the caller must ensure that ptr and len are valid.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

#[repr(C)]
pub struct KeyValue<K, V> {
    pub key: K,
    pub value: V,
}
#[repr(C)]
pub struct ArrayMap<K, V> {
    pub elements: *const KeyValue<K, V>,
    pub count: usize,
}
impl<K, V> ArrayMap<K, V> {
    /// # Safety
    /// - The returned value must not outlive `slice`.
    unsafe fn borrow_from_slice(slice: &[KeyValue<K, V>]) -> ArrayMap<K, V> {
        ArrayMap {
            elements: slice.as_ptr(),
            count: slice.len(),
        }
    }
}

impl BorrowedStr {
    /// Borrow string from `s`.
    ///
    /// # Safety
    ///
    /// - The returned value must non outlive `s`.
    /// - `s` must not be modified while `BorrowedStr` is alive.
    #[inline]
    unsafe fn borrow_from_str(s: &str) -> BorrowedStr {
        BorrowedStr {
            ptr: s.as_ptr(),
            len: s.len(),
        }
    }

    /// Borrow string from `s`.
    #[inline]
    fn borrow_static(s: &'static str) -> BorrowedStr {
        // SAFETY: static string outlives any scope, so borrowing from it is safe.
        unsafe { BorrowedStr::borrow_from_str(s) }
    }

    #[inline]
    const fn empty() -> BorrowedStr {
        BorrowedStr {
            ptr: std::ptr::null(),
            len: 0,
        }
    }
}

/// Get value produced by evaluation.
///
/// # Ownership
///
/// The returned `VariantValue` borrows from `assignment`. It must not be used after `assignment` is
/// freed.
///
/// # Safety
/// `assignment` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_value(
    assignment: Handle<ResolutionDetails>,
) -> VariantValue {
    // SAFETY: the caller must ensure that assignment is valid.
    match unsafe { assignment.as_ref() }.as_ref() {
        Ok(assignment) => match &assignment.value {
            AssignmentValue::String(s) => {
                // SAFETY: caller is required to not use return value after freeing
                // `assignment`.
                VariantValue::String(unsafe { BorrowedStr::borrow_from_str(s.as_str()) })
            }
            AssignmentValue::Integer(v) => VariantValue::Integer(*v),
            AssignmentValue::Float(v) => VariantValue::Float(*v),
            AssignmentValue::Boolean(v) => VariantValue::Boolean(*v),
            AssignmentValue::Json { value: _, raw } => {
                // SAFETY: caller is required to not use return value after freeing
                // `assignment`.
                VariantValue::Object(unsafe { BorrowedStr::borrow_from_str(raw.get()) })
            }
        },
        _ => VariantValue::None,
    }
}

/// Get variant key produced by evaluation. Returns `NULL` if evaluation did not produce any value.
///
/// # Ownership
///
/// The returned string borrows from `assignment`. It must not be used after `assignment` is
/// freed.
///
/// # Safety
/// `assignment` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_variant(
    assignment: Handle<ResolutionDetails>,
) -> BorrowedStr {
    // SAFETY: the caller must ensure that assignment is valid.
    match unsafe { assignment.as_ref() }.as_ref() {
        Ok(assignment) =>
        // SAFETY: caller is required to not use return value after freeing `assignment`.
        unsafe { BorrowedStr::borrow_from_str(&assignment.variation_key) },
        _ => BorrowedStr::empty(),
    }
}

/// Get allocation key produced by evaluation. Returns `NULL` if evaluation did not produce any
/// value.
///
/// # Ownership
///
/// The returned string borrows from `assignment`. It must not be used after `assignment` is
/// freed.
///
/// # Safety
/// `assignment` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_allocation_key(
    assignment: Handle<ResolutionDetails>,
) -> BorrowedStr {
    // SAFETY: the caller must ensure that assignment is valid.
    match unsafe { assignment.as_ref() }.as_ref() {
        // SAFETY: caller is required to not use return value after freeing
        // `assignment`.
        Ok(assignment) => unsafe {
            BorrowedStr::borrow_from_str(assignment.allocation_key.as_str())
        },
        _ => BorrowedStr::empty(),
    }
}

/// # Safety
/// `assignment` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_reason(
    assignment: Handle<ResolutionDetails>,
) -> Reason {
    // SAFETY: the caller must ensure that assignment is valid
    Reason::from(unsafe { assignment.as_ref() })
}
impl From<&ResolutionDetails> for Reason {
    fn from(value: &ResolutionDetails) -> Self {
        match value.as_ref() {
            Ok(assignment) => assignment.reason.into(),
            Err(EvaluationError::FlagDisabled) => Reason::Disabled,
            Err(EvaluationError::DefaultAllocationNull) => Reason::Default,
            Err(_) => Reason::Error,
        }
    }
}
impl From<AssignmentReason> for Reason {
    fn from(value: AssignmentReason) -> Self {
        match value {
            AssignmentReason::TargetingMatch => Reason::TargetingMatch,
            AssignmentReason::Split => Reason::Split,
            AssignmentReason::Default => Reason::Default,
            AssignmentReason::Static => Reason::Static,
        }
    }
}

/// # Safety
/// `assignment` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_error_code(
    assignment: Handle<ResolutionDetails>,
) -> ErrorCode {
    // SAFETY: the caller must ensure that assignment is valid
    ErrorCode::from(unsafe { assignment.as_ref() })
}
impl From<&ResolutionDetails> for ErrorCode {
    fn from(value: &ResolutionDetails) -> Self {
        match value.as_ref() {
            Ok(_) => ErrorCode::Ok,
            Err(err) => ErrorCode::from(err),
        }
    }
}
impl From<&EvaluationError> for ErrorCode {
    fn from(value: &EvaluationError) -> Self {
        match value {
            EvaluationError::TypeMismatch { .. } => ErrorCode::TypeMismatch,
            EvaluationError::TargetingKeyMissing => ErrorCode::TargetingKeyMissing,
            EvaluationError::ConfigurationParseError => ErrorCode::ParseError,
            EvaluationError::FlagConfigurationInvalid => ErrorCode::ParseError,
            EvaluationError::ConfigurationMissing => ErrorCode::ProviderNotReady,
            EvaluationError::FlagUnrecognizedOrDisabled => ErrorCode::FlagNotFound,
            EvaluationError::FlagDisabled => ErrorCode::Ok,
            EvaluationError::DefaultAllocationNull => ErrorCode::Ok,
            EvaluationError::Internal(_) => ErrorCode::General,
            _ => ErrorCode::General,
        }
    }
}

/// # Safety
/// `assignment` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_error_message(
    assignment: Handle<ResolutionDetails>,
) -> BorrowedStr {
    // SAFETY: the caller must ensure that assignment is valid
    let assignment = unsafe { assignment.as_ref() };
    match assignment.error_message.as_ref() {
        // SAFETY: the caller must not use returned value after assignment is freed.
        Some(s) => unsafe { BorrowedStr::borrow_from_str(s.as_str()) },
        None => BorrowedStr::empty(),
    }
}

/// Get the serial_id produced by evaluation.
///
/// Returns `None` if the assignment has no serial_id or evaluation did not produce any value.
///
/// # Safety
/// `assignment` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_serial_id(
    assignment: Handle<ResolutionDetails>,
) -> libdd_common_ffi::Option<i32> {
    // SAFETY: the caller must ensure that assignment handle is valid.
    match unsafe { assignment.as_ref() }.as_ref() {
        Ok(a) => a.serial_id.into(),
        Err(_) => libdd_common_ffi::Option::None,
    }
}

/// # Safety
/// `assignment` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_do_log(
    assignment: Handle<ResolutionDetails>,
) -> bool {
    // SAFETY: the caller must ensure that assignment handle is valid.
    match unsafe { assignment.as_ref() }.as_ref() {
        Ok(a) => a.do_log,
        Err(_) => false,
    }
}

/// # Safety
/// `assignment` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignnment_get_flag_metadata(
    assignment: Handle<ResolutionDetails>,
) -> ArrayMap<BorrowedStr, BorrowedStr> {
    // SAFETY: the caller must ensure that assignment is valid
    let a = unsafe { assignment.as_ref() };
    // SAFETY: the caller must ensure that returned value is not used after `assignment` is freed.
    unsafe { ArrayMap::borrow_from_slice(&a.flag_metadata) }
}

/// Frees an Assignment handle.
///
/// # Safety
/// - `assignment` must be a valid Assignment handle
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_drop(assignment: *mut Handle<ResolutionDetails>) {
    // SAFETY: the caller must ensure that assignment is valid
    unsafe { Handle::free(assignment) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prerequisite_handles_preserve_errors_eligibility_and_ownership() {
        let json = br#"{
          "createdAt":"2026-01-01T00:00:00Z","environment":{"name":"test"},
          "flags":{
            "parent":{"key":"parent","enabled":true,"variationType":"BOOLEAN",
              "variations":{"on":{"key":"on","value":true}},
              "allocations":[{"key":"parent-allocation","doLog":true,"splits":[{"variationKey":"on","shards":[]}]}]},
            "root":{"key":"root","enabled":true,"variationType":"BOOLEAN",
              "variations":{"on":{"key":"on","value":true}},
              "allocations":[{"key":"root-allocation","rules":[{"conditions":[
                {"flagEvaluation":{"key":"parent"},"operator":"ONE_OF","value":["on"]},
                {"flagEvaluation":{"key":"parent"},"operator":"ONE_OF","value":["on"]}
              ]}],"splits":[{"variationKey":"on","shards":[]}]}]}
          }}"#;
        for prerequisite in ["parent", "missing"] {
            let wire = String::from_utf8(json.to_vec()).unwrap().replace(
                "\"flagEvaluation\":{\"key\":\"parent\"}",
                &format!("\"flagEvaluation\":{{\"key\":\"{prerequisite}\"}}"),
            );
            let config = Configuration::from_server_response(
                ffe::UniversalFlagConfig::from_json(wire.into_bytes()).unwrap(),
            );
            let context = EvaluationContext::new(Some("subject".into()), Default::default());
            let trace = config.eval_flag_with_trace(
                "root",
                &context,
                ffe::ExpectedFlagType::Boolean,
                now(),
                Default::default(),
            );
            let mut details = ResolutionDetails::new(trace.result);
            details.dependencies = trace.evaluations;
            let mut root = Handle::new(details);
            // SAFETY: the handle copies are borrowed only until root is freed below.
            let count_handle = unsafe { std::ptr::read(&root) };
            assert_eq!(
                // SAFETY: count_handle borrows the live root.
                unsafe { ddog_ffe_assignment_dependency_count(count_handle) },
                1
            );
            // SAFETY: this copy borrows the live root.
            let key_handle = unsafe { std::ptr::read(&root) };
            // SAFETY: key_handle remains valid throughout this call.
            let key = unsafe { ddog_ffe_assignment_dependency_key(key_handle, 0) };
            assert_eq!(
                // SAFETY: the key is borrowed from the live root.
                unsafe { std::slice::from_raw_parts(key.ptr, key.len) },
                prerequisite.as_bytes()
            );
            // SAFETY: this copy borrows the live root.
            let eligibility_handle = unsafe { std::ptr::read(&root) };
            assert_eq!(
                // SAFETY: eligibility_handle remains valid throughout this call.
                unsafe { ddog_ffe_assignment_dependency_exposure_eligible(eligibility_handle, 0) },
                prerequisite == "parent"
            );
            // SAFETY: this copy borrows the live root.
            let result_handle = unsafe { std::ptr::read(&root) };
            // SAFETY: result_handle is live; the returned copy owns its result independently.
            let mut dependency = unsafe { ddog_ffe_assignment_dependency_result(result_handle, 0) };
            // SAFETY: all root borrows ended.
            unsafe { ddog_ffe_assignment_drop(&mut root) };
            // SAFETY: the child owns its result and survives the root's destruction.
            let error_handle = unsafe { std::ptr::read(&dependency) };
            assert_eq!(
                // SAFETY: error_handle borrows the live child.
                unsafe { ddog_ffe_assignment_get_error_code(error_handle) },
                if prerequisite == "parent" {
                    ErrorCode::Ok
                } else {
                    ErrorCode::FlagNotFound
                }
            );
            if prerequisite == "parent" {
                // SAFETY: this copy borrows the live child.
                let allocation_handle = unsafe { std::ptr::read(&dependency) };
                // SAFETY: allocation_handle remains valid throughout the call.
                let allocation =
                    unsafe { ddog_ffe_assignment_get_allocation_key(allocation_handle) };
                assert_eq!(
                    // SAFETY: allocation borrows the still-live child.
                    unsafe { std::slice::from_raw_parts(allocation.ptr, allocation.len) },
                    b"parent-allocation"
                );
            }
            // SAFETY: all child borrows ended; free its owned copy once.
            unsafe { ddog_ffe_assignment_drop(&mut dependency) };
        }
    }

    #[test]
    fn rejected_flag_is_exposed_as_parse_error() {
        let details = ResolutionDetails::from(EvaluationError::FlagConfigurationInvalid);

        assert!(matches!(Reason::from(&details), Reason::Error));
        assert_eq!(ErrorCode::from(&details), ErrorCode::ParseError);
    }
}
