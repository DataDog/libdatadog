// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{CStr, c_char};
use std::pin::Pin;

use libdd_ffe::rules_based as ffe;
use libdd_ffe::rules_based::{
    Assignment, AssignmentReason, AssignmentValue, Configuration, EvaluationContext,
    EvaluationError, EvaluationResult, Str, now,
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
    dependency_evaluations: Vec<DependencyEvaluation>,
    exposure_trace: Vec<Str>,
}
impl ResolutionDetails {
    fn new(value: Result<Assignment, EvaluationError>) -> ResolutionDetails {
        Self::from_evaluation_result(EvaluationResult {
            result: value,
            evaluations: Vec::new(),
            exposure_trace: Vec::new(),
        })
    }

    fn from_evaluation_result(value: EvaluationResult) -> ResolutionDetails {
        let EvaluationResult {
            result,
            evaluations,
            exposure_trace,
        } = value;
        // Pin the value to the heap to ensure its memory address remains stable
        let inner = Box::pin(result);

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
            dependency_evaluations: evaluations
                .into_iter()
                .map(|record| DependencyEvaluation {
                    flag_key: record.flag_key,
                    details: EvaluationDetails::new(record.result),
                    exposure_eligible: record.exposure_eligible,
                })
                .collect(),
            exposure_trace,
        }
    }
}

/// Borrowed view of a prerequisite evaluation owned by `ResolutionDetails`.
pub struct DependencyEvaluation {
    flag_key: Str,
    details: EvaluationDetails,
    exposure_eligible: bool,
}

struct EvaluationDetails {
    // Pin the result because metadata values borrow from assignment strings stored in it.
    inner: Pin<Box<Result<Assignment, EvaluationError>>>,
    error_message: Option<String>,
    flag_metadata: Vec<KeyValue<BorrowedStr, BorrowedStr>>,
}

impl EvaluationDetails {
    fn new(value: Result<Assignment, EvaluationError>) -> Self {
        let inner = Box::pin(value);
        let error_message = (*inner).as_ref().err().map(ToString::to_string);
        let mut flag_metadata = Vec::new();
        if let Ok(value) = (*inner).as_ref() {
            flag_metadata.push(KeyValue {
                key: BorrowedStr::borrow_static("__dd_allocation_key"),
                // SAFETY: `inner` is pinned for the lifetime of this owner and is only shared.
                value: unsafe { BorrowedStr::borrow_from_str(&value.allocation_key) },
            });
        }
        Self {
            inner,
            error_message,
            flag_metadata,
        }
    }
}

impl AsRef<Result<Assignment, EvaluationError>> for EvaluationDetails {
    fn as_ref(&self) -> &Result<Assignment, EvaluationError> {
        &self.inner
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

    let evaluation_result = config.eval_flag_with_trace(
        flag_key,
        context,
        expected_type.into(),
        now(),
        ffe::EvaluationOptions::default(),
    );

    Handle::new(ResolutionDetails::from_evaluation_result(evaluation_result))
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
    const fn empty() -> Self {
        Self {
            elements: std::ptr::null(),
            count: 0,
        }
    }

    /// # Safety
    /// - The returned value must not outlive `slice`.
    unsafe fn borrow_from_slice(slice: &[KeyValue<K, V>]) -> ArrayMap<K, V> {
        ArrayMap {
            elements: slice.as_ptr(),
            count: slice.len(),
        }
    }
}

/// # Safety
/// The returned value must not outlive `value`.
unsafe fn assignment_value(value: &AssignmentValue) -> VariantValue {
    match value {
        AssignmentValue::String(value) => {
            // SAFETY: the caller guarantees that the returned value does not outlive `value`.
            VariantValue::String(unsafe { BorrowedStr::borrow_from_str(value) })
        }
        AssignmentValue::Integer(value) => VariantValue::Integer(*value),
        AssignmentValue::Float(value) => VariantValue::Float(*value),
        AssignmentValue::Boolean(value) => VariantValue::Boolean(*value),
        AssignmentValue::Json { value: _, raw } => {
            // SAFETY: the caller guarantees that the returned value does not outlive `value`.
            VariantValue::Object(unsafe { BorrowedStr::borrow_from_str(raw.get()) })
        }
    }
}

fn reason_from_result(result: &Result<Assignment, EvaluationError>) -> Reason {
    match result {
        Ok(assignment) => assignment.reason.into(),
        Err(EvaluationError::FlagDisabled) => Reason::Disabled,
        Err(EvaluationError::DefaultAllocationNull) => Reason::Default,
        Err(_) => Reason::Error,
    }
}

fn error_code_from_result(result: &Result<Assignment, EvaluationError>) -> ErrorCode {
    match result {
        Ok(_) => ErrorCode::Ok,
        Err(error) => ErrorCode::from(error),
    }
}

fn dependency_error_code(error: &EvaluationError) -> ErrorCode {
    match error {
        EvaluationError::DependencyEvaluationFailed { source, .. } => dependency_error_code(source),
        EvaluationError::FlagDisabled | EvaluationError::DefaultAllocationNull => {
            ErrorCode::General
        }
        error => ErrorCode::from(error),
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
        // SAFETY: caller is required not to use the returned value after freeing `assignment`.
        Ok(assignment) => unsafe { assignment_value(&assignment.value) },
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
        reason_from_result(value.as_ref())
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
        error_code_from_result(value.as_ref())
    }
}
impl From<&EvaluationError> for ErrorCode {
    fn from(value: &EvaluationError) -> Self {
        match value {
            EvaluationError::DependencyEvaluationFailed { source, .. } => {
                dependency_error_code(source)
            }
            EvaluationError::TypeMismatch { .. } => ErrorCode::TypeMismatch,
            EvaluationError::TargetingKeyMissing => ErrorCode::TargetingKeyMissing,
            EvaluationError::InvalidContext => ErrorCode::InvalidContext,
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

/// Number of completed prerequisite evaluations returned with this root evaluation.
///
/// Records are in completion order and exclude the requested root flag.
///
/// # Safety
/// `assignment` must be a valid handle.
// allow(ffi-panic-boundary): shared length accessor under the documented valid-handle precondition
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_dependency_evaluation_count(
    assignment: Handle<ResolutionDetails>,
) -> usize {
    // SAFETY: the caller must ensure that assignment is valid.
    unsafe { assignment.as_ref() }.dependency_evaluations.len()
}

/// Get a borrowed prerequisite evaluation by completion-order index.
///
/// Returns `NULL` when `index` is out of bounds. The returned pointer is owned by `assignment`
/// and must not be freed or used after `assignment` is dropped.
///
/// # Safety
/// `assignment` must be a valid handle.
// allow(ffi-panic-boundary): checked index accessor under the documented valid-handle precondition
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_dependency_evaluation(
    assignment: Handle<ResolutionDetails>,
    index: usize,
) -> *const DependencyEvaluation {
    // SAFETY: the caller must ensure that assignment is valid.
    unsafe { assignment.as_ref() }
        .dependency_evaluations
        .get(index)
        .map_or(std::ptr::null(), std::ptr::from_ref)
}

/// Get the flag key for a borrowed prerequisite evaluation.
///
/// Returns an empty string for `NULL`. The returned string remains valid until the owning root
/// assignment is dropped.
///
/// # Safety
/// A non-NULL `evaluation` must have been returned by
/// `ddog_ffe_assignment_get_dependency_evaluation` from a live assignment.
// allow(ffi-panic-boundary): null-checked shared string accessor has no panicking operations
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_dependency_evaluation_get_flag_key(
    evaluation: *const DependencyEvaluation,
) -> BorrowedStr {
    // SAFETY: the caller guarantees that a non-NULL pointer is valid and its owner is live.
    let Some(evaluation) = (unsafe { evaluation.as_ref() }) else {
        return BorrowedStr::empty();
    };
    // SAFETY: the returned borrow is bounded by the owning assignment's lifetime.
    unsafe { BorrowedStr::borrow_from_str(&evaluation.flag_key) }
}

/// Get the value produced by a borrowed prerequisite evaluation.
///
/// # Safety
/// A non-NULL `evaluation` must be a live borrowed prerequisite evaluation.
// allow(ffi-panic-boundary): null-checked shared value accessor has no panicking operations
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_dependency_evaluation_get_value(
    evaluation: *const DependencyEvaluation,
) -> VariantValue {
    // SAFETY: the caller guarantees that a non-NULL pointer is valid and its owner is live.
    let Some(evaluation) = (unsafe { evaluation.as_ref() }) else {
        return VariantValue::None;
    };
    match evaluation.details.as_ref() {
        // SAFETY: the returned borrow is bounded by the owning assignment's lifetime.
        Ok(assignment) => unsafe { assignment_value(&assignment.value) },
        Err(_) => VariantValue::None,
    }
}

/// Get the selected variant key for a borrowed prerequisite evaluation.
///
/// # Safety
/// A non-NULL `evaluation` must be a live borrowed prerequisite evaluation.
// allow(ffi-panic-boundary): null-checked shared string accessor has no panicking operations
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_dependency_evaluation_get_variant(
    evaluation: *const DependencyEvaluation,
) -> BorrowedStr {
    // SAFETY: the caller guarantees that a non-NULL pointer is valid and its owner is live.
    let Some(evaluation) = (unsafe { evaluation.as_ref() }) else {
        return BorrowedStr::empty();
    };
    match evaluation.details.as_ref() {
        // SAFETY: the returned borrow is bounded by the owning assignment's lifetime.
        Ok(assignment) => unsafe { BorrowedStr::borrow_from_str(&assignment.variation_key) },
        Err(_) => BorrowedStr::empty(),
    }
}

/// Get the selected allocation key for a borrowed prerequisite evaluation.
///
/// # Safety
/// A non-NULL `evaluation` must be a live borrowed prerequisite evaluation.
// allow(ffi-panic-boundary): null-checked shared string accessor has no panicking operations
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_dependency_evaluation_get_allocation_key(
    evaluation: *const DependencyEvaluation,
) -> BorrowedStr {
    // SAFETY: the caller guarantees that a non-NULL pointer is valid and its owner is live.
    let Some(evaluation) = (unsafe { evaluation.as_ref() }) else {
        return BorrowedStr::empty();
    };
    match evaluation.details.as_ref() {
        // SAFETY: the returned borrow is bounded by the owning assignment's lifetime.
        Ok(assignment) => unsafe { BorrowedStr::borrow_from_str(&assignment.allocation_key) },
        Err(_) => BorrowedStr::empty(),
    }
}

/// # Safety
/// A non-NULL `evaluation` must be a live borrowed prerequisite evaluation.
// allow(ffi-panic-boundary): null-checked enum mapping has no panicking operations
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_dependency_evaluation_get_reason(
    evaluation: *const DependencyEvaluation,
) -> Reason {
    // SAFETY: the caller guarantees that a non-NULL pointer is valid and its owner is live.
    let Some(evaluation) = (unsafe { evaluation.as_ref() }) else {
        return Reason::Error;
    };
    match evaluation.details.as_ref() {
        Ok(assignment) => assignment.reason.into(),
        Err(_) => Reason::Error,
    }
}

/// # Safety
/// A non-NULL `evaluation` must be a live borrowed prerequisite evaluation.
// allow(ffi-panic-boundary): null-checked enum mapping has no panicking operations
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_dependency_evaluation_get_error_code(
    evaluation: *const DependencyEvaluation,
) -> ErrorCode {
    // SAFETY: the caller guarantees that a non-NULL pointer is valid and its owner is live.
    let Some(evaluation) = (unsafe { evaluation.as_ref() }) else {
        return ErrorCode::General;
    };
    match evaluation.details.as_ref() {
        Ok(_) => ErrorCode::Ok,
        Err(error) => dependency_error_code(error),
    }
}

/// Get the error message for a borrowed prerequisite evaluation.
///
/// # Safety
/// A non-NULL `evaluation` must be a live borrowed prerequisite evaluation.
// allow(ffi-panic-boundary): null-checked shared string accessor has no panicking operations
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_dependency_evaluation_get_error_message(
    evaluation: *const DependencyEvaluation,
) -> BorrowedStr {
    // SAFETY: the caller guarantees that a non-NULL pointer is valid and its owner is live.
    let Some(evaluation) = (unsafe { evaluation.as_ref() }) else {
        return BorrowedStr::empty();
    };
    match evaluation.details.as_ref() {
        Err(EvaluationError::FlagDisabled | EvaluationError::DefaultAllocationNull) => {
            BorrowedStr::borrow_static("prerequisite has no assignment")
        }
        _ => match &evaluation.details.error_message {
            // SAFETY: the returned borrow is bounded by the owning assignment's lifetime.
            Some(message) => unsafe { BorrowedStr::borrow_from_str(message) },
            None => BorrowedStr::empty(),
        },
    }
}

/// # Safety
/// A non-NULL `evaluation` must be a live borrowed prerequisite evaluation.
// allow(ffi-panic-boundary): null-checked scalar accessor has no panicking operations
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_dependency_evaluation_get_serial_id(
    evaluation: *const DependencyEvaluation,
) -> libdd_common_ffi::Option<i32> {
    // SAFETY: the caller guarantees that a non-NULL pointer is valid and its owner is live.
    let Some(evaluation) = (unsafe { evaluation.as_ref() }) else {
        return libdd_common_ffi::Option::None;
    };
    match evaluation.details.as_ref() {
        Ok(assignment) => assignment.serial_id.into(),
        Err(_) => libdd_common_ffi::Option::None,
    }
}

/// # Safety
/// A non-NULL `evaluation` must be a live borrowed prerequisite evaluation.
// allow(ffi-panic-boundary): null-checked scalar accessor has no panicking operations
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_dependency_evaluation_get_do_log(
    evaluation: *const DependencyEvaluation,
) -> bool {
    // SAFETY: the caller guarantees that a non-NULL pointer is valid and its owner is live.
    let Some(evaluation) = (unsafe { evaluation.as_ref() }) else {
        return false;
    };
    matches!(evaluation.details.as_ref(), Ok(assignment) if assignment.do_log)
}

/// Whether this prerequisite belongs to the successful root exposure path.
///
/// This does not override `do_log` or SDK exposure consent and deduplication policy.
///
/// # Safety
/// A non-NULL `evaluation` must be a live borrowed prerequisite evaluation.
// allow(ffi-panic-boundary): null-checked scalar accessor has no panicking operations
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_dependency_evaluation_get_exposure_eligible(
    evaluation: *const DependencyEvaluation,
) -> bool {
    // SAFETY: the caller guarantees that a non-NULL pointer is valid and its owner is live.
    (unsafe { evaluation.as_ref() }).is_some_and(|evaluation| evaluation.exposure_eligible)
}

/// Get flag metadata for a borrowed prerequisite evaluation.
///
/// # Safety
/// A non-NULL `evaluation` must be a live borrowed prerequisite evaluation.
// allow(ffi-panic-boundary): null-checked shared slice accessor has no panicking operations
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_dependency_evaluation_get_flag_metadata(
    evaluation: *const DependencyEvaluation,
) -> ArrayMap<BorrowedStr, BorrowedStr> {
    // SAFETY: the caller guarantees that a non-NULL pointer is valid and its owner is live.
    let Some(evaluation) = (unsafe { evaluation.as_ref() }) else {
        return ArrayMap::empty();
    };
    // SAFETY: the returned borrow is bounded by the owning assignment's lifetime.
    unsafe { ArrayMap::borrow_from_slice(&evaluation.details.flag_metadata) }
}

/// Number of prerequisite keys in the successful root exposure path.
///
/// # Safety
/// `assignment` must be a valid handle.
// allow(ffi-panic-boundary): shared length accessor under the documented valid-handle precondition
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_exposure_trace_count(
    assignment: Handle<ResolutionDetails>,
) -> usize {
    // SAFETY: the caller must ensure that assignment is valid.
    unsafe { assignment.as_ref() }.exposure_trace.len()
}

/// Get one successful-path prerequisite key in depth-first authored order.
///
/// Returns an empty string when `index` is out of bounds. The returned string remains valid until
/// the owning root assignment is dropped.
///
/// # Safety
/// `assignment` must be a valid handle.
// allow(ffi-panic-boundary): checked index accessor under the documented valid-handle precondition
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_exposure_trace_flag_key(
    assignment: Handle<ResolutionDetails>,
    index: usize,
) -> BorrowedStr {
    // SAFETY: the caller must ensure that assignment is valid.
    let Some(flag_key) = unsafe { assignment.as_ref() }.exposure_trace.get(index) else {
        return BorrowedStr::empty();
    };
    // SAFETY: the returned borrow is bounded by the owning assignment's lifetime.
    unsafe { BorrowedStr::borrow_from_str(flag_key) }
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

    fn assignment(variant: &str, allocation: &str, do_log: bool) -> Assignment {
        assignment_with_value(AssignmentValue::Boolean(true), variant, allocation, do_log)
    }

    fn assignment_with_value(
        value: AssignmentValue,
        variant: &str,
        allocation: &str,
        do_log: bool,
    ) -> Assignment {
        Assignment {
            value,
            variation_key: variant.into(),
            allocation_key: allocation.into(),
            reason: AssignmentReason::TargetingMatch,
            serial_id: Some(7),
            do_log,
        }
    }

    unsafe fn copy_handle<T>(handle: &Handle<T>) -> Handle<T> {
        // SAFETY: tests keep the original allocation live and free only the original handle.
        unsafe { std::ptr::read(handle) }
    }

    fn call_handle<R>(
        handle: &Handle<ResolutionDetails>,
        function: unsafe extern "C" fn(Handle<ResolutionDetails>) -> R,
    ) -> R {
        // SAFETY: the original handle remains live and is the only copy that will be freed.
        let copied = unsafe { copy_handle(handle) };
        // SAFETY: `copied` points to the same live allocation as `handle`.
        unsafe { function(copied) }
    }

    fn call_handle_at<R>(
        handle: &Handle<ResolutionDetails>,
        index: usize,
        function: unsafe extern "C" fn(Handle<ResolutionDetails>, usize) -> R,
    ) -> R {
        // SAFETY: the original handle remains live and is the only copy that will be freed.
        let copied = unsafe { copy_handle(handle) };
        // SAFETY: `copied` points to the same live allocation as `handle`.
        unsafe { function(copied, index) }
    }

    fn call_evaluation<R>(
        evaluation: *const DependencyEvaluation,
        function: unsafe extern "C" fn(*const DependencyEvaluation) -> R,
    ) -> R {
        // SAFETY: tests pass NULL or a borrowed pointer whose parent remains live.
        unsafe { function(evaluation) }
    }

    fn borrowed_str(value: BorrowedStr) -> String {
        // SAFETY: tests only call this while the owner of the borrowed value is live.
        String::from_utf8(unsafe { value.as_bytes() }.to_vec()).unwrap()
    }

    #[test]
    fn returned_batch_ffi_preserves_order_bounds_and_root_compatibility() {
        let result = EvaluationResult {
            result: Ok(assignment("root-on", "root-allocation", true)),
            evaluations: vec![
                ffe::EvaluationRecord {
                    flag_key: "leaf".into(),
                    result: Ok(assignment("leaf-on", "leaf-allocation", false)),
                    exposure_eligible: true,
                },
                ffe::EvaluationRecord {
                    flag_key: "parent".into(),
                    result: Err(EvaluationError::Internal("dependency failed".into())),
                    exposure_eligible: false,
                },
            ],
            exposure_trace: vec!["parent".into(), "leaf".into()],
        };
        let mut handle = Handle::new(ResolutionDetails::from_evaluation_result(result));

        // Existing root accessors continue to operate on the root result.
        assert_eq!(
            call_handle(&handle, ddog_ffe_assignment_get_error_code),
            ErrorCode::Ok
        );
        assert_eq!(
            borrowed_str(call_handle(&handle, ddog_ffe_assignment_get_variant)),
            "root-on"
        );

        assert_eq!(
            call_handle(&handle, ddog_ffe_assignment_get_dependency_evaluation_count,),
            2
        );
        let leaf = call_handle_at(&handle, 0, ddog_ffe_assignment_get_dependency_evaluation);
        let parent = call_handle_at(&handle, 1, ddog_ffe_assignment_get_dependency_evaluation);
        assert!(!leaf.is_null());
        assert!(!parent.is_null());
        assert!(
            call_handle_at(&handle, 2, ddog_ffe_assignment_get_dependency_evaluation).is_null()
        );

        assert_eq!(
            borrowed_str(call_evaluation(
                leaf,
                ddog_ffe_dependency_evaluation_get_flag_key,
            )),
            "leaf"
        );
        assert_eq!(
            borrowed_str(call_evaluation(
                leaf,
                ddog_ffe_dependency_evaluation_get_variant,
            )),
            "leaf-on"
        );
        assert_eq!(
            borrowed_str(call_evaluation(
                leaf,
                ddog_ffe_dependency_evaluation_get_allocation_key,
            )),
            "leaf-allocation"
        );
        assert!(!call_evaluation(
            leaf,
            ddog_ffe_dependency_evaluation_get_do_log
        ));
        assert!(call_evaluation(
            leaf,
            ddog_ffe_dependency_evaluation_get_exposure_eligible
        ));
        assert_eq!(
            call_evaluation(leaf, ddog_ffe_dependency_evaluation_get_error_code),
            ErrorCode::Ok
        );
        let metadata = call_evaluation(leaf, ddog_ffe_dependency_evaluation_get_flag_metadata);
        assert_eq!(metadata.count, 1);
        assert_eq!(
            call_evaluation(leaf, ddog_ffe_dependency_evaluation_get_serial_id),
            libdd_common_ffi::Option::Some(7)
        );
        assert!(matches!(
            call_evaluation(leaf, ddog_ffe_dependency_evaluation_get_reason),
            Reason::TargetingMatch
        ));

        // Failed records retain their key and owned error text without a value.
        assert_eq!(
            call_evaluation(parent, ddog_ffe_dependency_evaluation_get_error_code),
            ErrorCode::General
        );
        assert_eq!(
            borrowed_str(call_evaluation(
                parent,
                ddog_ffe_dependency_evaluation_get_error_message,
            )),
            "internal error: dependency failed"
        );
        assert!(matches!(
            call_evaluation(parent, ddog_ffe_dependency_evaluation_get_value),
            VariantValue::None
        ));

        // Exposure trace ordering is independent from evaluation completion ordering.
        assert_eq!(
            call_handle(&handle, ddog_ffe_assignment_get_exposure_trace_count),
            2
        );
        assert_eq!(
            borrowed_str(call_handle_at(
                &handle,
                0,
                ddog_ffe_assignment_get_exposure_trace_flag_key,
            )),
            "parent"
        );
        assert_eq!(
            call_handle_at(&handle, 2, ddog_ffe_assignment_get_exposure_trace_flag_key,).len,
            0
        );

        // NULL child access is defined and never dereferences memory.
        assert_eq!(
            call_evaluation(
                std::ptr::null(),
                ddog_ffe_dependency_evaluation_get_flag_key,
            )
            .len,
            0
        );
        // SAFETY: all borrows ended; the parent is the sole owner and is freed exactly once.
        unsafe { ddog_ffe_assignment_drop(&mut handle) };
    }

    #[test]
    fn returned_batch_ffi_exposes_depth_terminal_before_its_ancestor() {
        let wire = ffe::UniversalFlagConfig::from_json(
            br#"{
                "createdAt":"2026-10-28T00:00:00Z",
                "environment":{"name":"test"},
                "evaluatorParams":{"maxDependencyDepth":1},
                "flags":{
                    "root":{"key":"root","enabled":true,"variationType":"BOOLEAN","variations":{"on":{"key":"on","value":true}},"allocations":[{"key":"root-a","rules":[{"conditions":[{"flagEvaluation":{"key":"f1"},"operator":"ONE_OF","value":["on"]}]}],"splits":[{"variationKey":"on","shards":[]}]}]},
                    "f1":{"key":"f1","enabled":true,"variationType":"BOOLEAN","variations":{"on":{"key":"on","value":true}},"allocations":[{"key":"f1-a","rules":[{"conditions":[{"flagEvaluation":{"key":"f2"},"operator":"ONE_OF","value":["on"]}]}],"splits":[{"variationKey":"on","shards":[]}]}]},
                    "f2":{"key":"f2","enabled":true,"variationType":"BOOLEAN","variations":{"on":{"key":"on","value":true}},"allocations":[{"key":"f2-a","splits":[{"variationKey":"on","shards":[]}]}]}
                }
            }"#
            .to_vec(),
        )
        .unwrap();
        let config = Configuration::from_server_response(wire);
        let result = config.eval_flag_with_trace(
            "root",
            &EvaluationContext::new(Some("subject".into()), Default::default()),
            ffe::ExpectedFlagType::Boolean,
            now(),
            ffe::EvaluationOptions::default(),
        );
        let mut handle = Handle::new(ResolutionDetails::from_evaluation_result(result));

        assert_eq!(
            call_handle(&handle, ddog_ffe_assignment_get_dependency_evaluation_count),
            2
        );
        let terminal = call_handle_at(&handle, 0, ddog_ffe_assignment_get_dependency_evaluation);
        let ancestor = call_handle_at(&handle, 1, ddog_ffe_assignment_get_dependency_evaluation);
        assert_eq!(
            borrowed_str(call_evaluation(
                terminal,
                ddog_ffe_dependency_evaluation_get_flag_key,
            )),
            "f2"
        );
        assert_eq!(
            borrowed_str(call_evaluation(
                terminal,
                ddog_ffe_dependency_evaluation_get_error_message,
            )),
            "flag dependency depth exceeded"
        );
        assert_eq!(
            borrowed_str(call_evaluation(
                ancestor,
                ddog_ffe_dependency_evaluation_get_flag_key,
            )),
            "f1"
        );
        assert_eq!(
            borrowed_str(call_evaluation(
                ancestor,
                ddog_ffe_dependency_evaluation_get_error_message,
            )),
            "prerequisite f2 failed: flag dependency depth exceeded"
        );
        // SAFETY: all child borrows ended before the sole owner is freed.
        unsafe { ddog_ffe_assignment_drop(&mut handle) };
    }

    #[test]
    fn failed_root_retains_dependency_records_and_has_no_exposure_trace() {
        let result = EvaluationResult {
            result: Err(EvaluationError::DependencyCycle),
            evaluations: vec![ffe::EvaluationRecord {
                flag_key: "completed".into(),
                result: Ok(assignment("on", "allocation", true)),
                exposure_eligible: false,
            }],
            exposure_trace: Vec::new(),
        };
        let mut handle = Handle::new(ResolutionDetails::from_evaluation_result(result));
        assert_eq!(
            call_handle(&handle, ddog_ffe_assignment_get_error_code),
            ErrorCode::General
        );
        assert_eq!(
            call_handle(&handle, ddog_ffe_assignment_get_dependency_evaluation_count,),
            1
        );
        assert_eq!(
            call_handle(&handle, ddog_ffe_assignment_get_exposure_trace_count),
            0
        );
        // SAFETY: all borrows ended and the parent owns every child record.
        unsafe { ddog_ffe_assignment_drop(&mut handle) };
    }

    #[test]
    fn dependency_no_assignment_errors_do_not_reuse_legacy_root_mappings() {
        for error in [
            EvaluationError::FlagDisabled,
            EvaluationError::DefaultAllocationNull,
        ] {
            let result = EvaluationResult {
                result: Ok(assignment("root", "root", false)),
                evaluations: vec![ffe::EvaluationRecord {
                    flag_key: "dependency".into(),
                    result: Err(error.clone()),
                    exposure_eligible: false,
                }],
                exposure_trace: Vec::new(),
            };
            let mut handle = Handle::new(ResolutionDetails::from_evaluation_result(result));
            let dependency =
                call_handle_at(&handle, 0, ddog_ffe_assignment_get_dependency_evaluation);
            assert!(matches!(
                call_evaluation(dependency, ddog_ffe_dependency_evaluation_get_reason),
                Reason::Error
            ));
            assert_eq!(
                call_evaluation(dependency, ddog_ffe_dependency_evaluation_get_error_code,),
                ErrorCode::General
            );
            assert_eq!(
                borrowed_str(call_evaluation(
                    dependency,
                    ddog_ffe_dependency_evaluation_get_error_message,
                )),
                "prerequisite has no assignment"
            );
            // SAFETY: all child borrows ended before the sole owner is freed.
            unsafe { ddog_ffe_assignment_drop(&mut handle) };

            let root = ResolutionDetails::from(error);
            assert!(matches!(
                Reason::from(&root),
                Reason::Disabled | Reason::Default
            ));
            assert_eq!(ErrorCode::from(&root), ErrorCode::Ok);
        }
    }

    #[test]
    fn propagated_dependency_errors_preserve_the_leaf_error_code() {
        let propagated = EvaluationError::DependencyEvaluationFailed {
            flag_key: "missing".into(),
            source: Box::new(EvaluationError::FlagUnrecognizedOrDisabled),
        };
        let result = EvaluationResult {
            result: Err(propagated.clone()),
            evaluations: vec![ffe::EvaluationRecord {
                flag_key: "ancestor".into(),
                result: Err(propagated),
                exposure_eligible: false,
            }],
            exposure_trace: Vec::new(),
        };
        let mut handle = Handle::new(ResolutionDetails::from_evaluation_result(result));
        assert_eq!(
            call_handle(&handle, ddog_ffe_assignment_get_error_code),
            ErrorCode::FlagNotFound
        );
        let ancestor = call_handle_at(&handle, 0, ddog_ffe_assignment_get_dependency_evaluation);
        assert_eq!(
            call_evaluation(ancestor, ddog_ffe_dependency_evaluation_get_error_code),
            ErrorCode::FlagNotFound
        );
        // SAFETY: all child borrows ended before the sole owner is freed.
        unsafe { ddog_ffe_assignment_drop(&mut handle) };
    }

    #[test]
    fn dependency_values_cover_every_supported_ffi_variant() {
        let wire = ffe::UniversalFlagConfig::from_json(
            br#"{
                "createdAt":"2026-10-28T00:00:00Z", "environment":{"name":"test"},
                "flags":{
                    "string":{"key":"string","enabled":true,"variationType":"STRING","variations":{"v":{"key":"v","value":"text"}},"allocations":[{"key":"a","doLog":true,"splits":[{"variationKey":"v","shards":[]}]}]},
                    "integer":{"key":"integer","enabled":true,"variationType":"INTEGER","variations":{"v":{"key":"v","value":42}},"allocations":[{"key":"a","doLog":true,"splits":[{"variationKey":"v","shards":[]}]}]},
                    "float":{"key":"float","enabled":true,"variationType":"NUMERIC","variations":{"v":{"key":"v","value":1.5}},"allocations":[{"key":"a","doLog":true,"splits":[{"variationKey":"v","shards":[]}]}]},
                    "boolean":{"key":"boolean","enabled":true,"variationType":"BOOLEAN","variations":{"v":{"key":"v","value":true}},"allocations":[{"key":"a","doLog":true,"splits":[{"variationKey":"v","shards":[]}]}]},
                    "object":{"key":"object","enabled":true,"variationType":"JSON","variations":{"v":{"key":"v","value":{"key":"value"}}},"allocations":[{"key":"a","doLog":true,"splits":[{"variationKey":"v","shards":[]}]}]}
                }
            }"#
            .to_vec(),
        )
        .unwrap();
        let config = Configuration::from_server_response(wire);
        let context = EvaluationContext::new(Some("subject".into()), Default::default());
        let result = EvaluationResult {
            result: Ok(assignment("root", "root", false)),
            evaluations: ["string", "integer", "float", "boolean", "object"]
                .into_iter()
                .map(|flag_key| ffe::EvaluationRecord {
                    flag_key: flag_key.into(),
                    result: config.eval_flag(flag_key, &context, ffe::ExpectedFlagType::Any, now()),
                    exposure_eligible: true,
                })
                .collect(),
            exposure_trace: Vec::new(),
        };
        let mut handle = Handle::new(ResolutionDetails::from_evaluation_result(result));
        let records = (0..5)
            .map(|index| {
                call_handle_at(
                    &handle,
                    index,
                    ddog_ffe_assignment_get_dependency_evaluation,
                )
            })
            .collect::<Vec<_>>();

        let VariantValue::String(value) =
            call_evaluation(records[0], ddog_ffe_dependency_evaluation_get_value)
        else {
            panic!("expected string dependency value");
        };
        assert_eq!(borrowed_str(value), "text");
        assert!(matches!(
            call_evaluation(records[1], ddog_ffe_dependency_evaluation_get_value),
            VariantValue::Integer(42)
        ));
        assert!(matches!(
            call_evaluation(records[2], ddog_ffe_dependency_evaluation_get_value),
            VariantValue::Float(value) if value == 1.5
        ));
        assert!(matches!(
            call_evaluation(records[3], ddog_ffe_dependency_evaluation_get_value),
            VariantValue::Boolean(true)
        ));
        let VariantValue::Object(value) =
            call_evaluation(records[4], ddog_ffe_dependency_evaluation_get_value)
        else {
            panic!("expected object dependency value");
        };
        assert_eq!(borrowed_str(value), r#"{"key":"value"}"#);
        // SAFETY: all borrowed record pointers are dead before the sole owning parent is freed.
        unsafe { ddog_ffe_assignment_drop(&mut handle) };
    }

    #[test]
    fn rejected_flag_is_exposed_as_parse_error() {
        let details = ResolutionDetails::from(EvaluationError::FlagConfigurationInvalid);

        assert!(matches!(Reason::from(&details), Reason::Error));
        assert_eq!(ErrorCode::from(&details), ErrorCode::ParseError);
    }
}
