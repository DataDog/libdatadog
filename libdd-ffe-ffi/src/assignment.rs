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
}
impl ResolutionDetails {
    fn new(value: Result<Assignment, EvaluationError>) -> ResolutionDetails {
        Self::from_evaluation_result(EvaluationResult {
            result: value,
            evaluations: Vec::new(),
        })
    }

    fn from_evaluation_result(value: EvaluationResult) -> ResolutionDetails {
        let EvaluationResult {
            result,
            evaluations,
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
    // Metadata values borrow from assignment strings, so keep the result at a stable address.
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
                // SAFETY: `inner` is pinned for this owner's lifetime and is only shared.
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
    // SAFETY: this function has the same handle and string requirements.
    unsafe {
        ddog_ffe_get_assignment_with_options(
            config,
            flag_key,
            expected_type,
            context,
            EvaluationOptions::default(),
        )
    }
}

/// Optional behavior selected by the SDK at provider construction.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct EvaluationOptions {
    pub experimentation_opt_out_immediate: bool,
}

/// SDK-selected handling of a subject's experimentation opt-out timestamp.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub enum ExperimentationOptOutMode {
    /// Ignore the subject's opt-out attribute.
    #[default]
    Disabled,
    /// Exclude exposure-logging allocations immediately.
    Immediate,
    /// Exclude allocations activated at or after opt-out, or with unknown activation.
    FutureExperimentsOnly,
}

impl From<ExperimentationOptOutMode> for ffe::ExperimentationOptOutMode {
    fn from(mode: ExperimentationOptOutMode) -> Self {
        match mode {
            ExperimentationOptOutMode::Disabled => Self::Disabled,
            ExperimentationOptOutMode::Immediate => Self::Immediate,
            ExperimentationOptOutMode::FutureExperimentsOnly => Self::FutureExperimentsOnly,
        }
    }
}

/// Evaluate a flag with optional subject experimentation exclusion.
///
/// # Ownership
/// The caller must free the returned handle with `ddog_ffe_assignment_drop`.
///
/// # Safety
/// `config` and `context` must be valid handles. `flag_key` must be a valid
/// NUL-terminated string.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn ddog_ffe_get_assignment_with_options(
    config: Handle<Configuration>,
    flag_key: *const c_char,
    expected_type: ExpectedFlagType,
    context: Handle<EvaluationContext>,
    options: EvaluationOptions,
) -> Handle<ResolutionDetails> {
    let mode = if options.experimentation_opt_out_immediate {
        ExperimentationOptOutMode::Immediate
    } else {
        ExperimentationOptOutMode::Disabled
    };
    // SAFETY: this function has the same handle and string requirements.
    unsafe {
        ddog_ffe_get_assignment_with_opt_out_mode(config, flag_key, expected_type, context, mode)
    }
}

/// Evaluate a flag with SDK-selected experimentation opt-out behavior.
///
/// # Ownership
/// The caller must free the returned handle with `ddog_ffe_assignment_drop`.
///
/// # Safety
/// `config` and `context` must be valid handles. `flag_key` must be a valid
/// NUL-terminated string. `mode` must be a valid enum value.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn ddog_ffe_get_assignment_with_opt_out_mode(
    config: Handle<Configuration>,
    flag_key: *const c_char,
    expected_type: ExpectedFlagType,
    context: Handle<EvaluationContext>,
    mode: ExperimentationOptOutMode,
) -> Handle<ResolutionDetails> {
    catch_assignment_panic(|| {
        // SAFETY: the caller provides valid handles and a valid string.
        unsafe { get_assignment_with_mode(config, flag_key, expected_type, context, mode) }
    })
}

// Other FFI crates have private catch_panic! macros tied to their own error types.
// Keep this boundary's handle ownership and redacted error conversion together.
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

unsafe fn get_assignment_with_mode(
    config: Handle<Configuration>,
    flag_key: *const c_char,
    expected_type: ExpectedFlagType,
    context: Handle<EvaluationContext>,
    mode: ExperimentationOptOutMode,
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
        ffe::EvaluationOptions {
            experimentation_opt_out_mode: mode.into(),
        },
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

/// Convert an assignment value into its borrowed C representation.
///
/// # Safety
/// The returned value must not outlive `value`.
unsafe fn assignment_value(value: &AssignmentValue) -> VariantValue {
    match value {
        AssignmentValue::String(value) => {
            // SAFETY: the caller bounds the returned borrow by `value`.
            VariantValue::String(unsafe { BorrowedStr::borrow_from_str(value) })
        }
        AssignmentValue::Integer(value) => VariantValue::Integer(*value),
        AssignmentValue::Float(value) => VariantValue::Float(*value),
        AssignmentValue::Boolean(value) => VariantValue::Boolean(*value),
        AssignmentValue::Json { value: _, raw } => {
            // SAFETY: the caller bounds the returned borrow by `value`.
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
        match value.as_ref() {
            Ok(_) => ErrorCode::Ok,
            Err(err) => ErrorCode::from(err),
        }
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

/// Number of prerequisite evaluations returned with this root evaluation.
///
/// # Safety
/// `assignment` must be a valid handle.
// allow(ffi-panic-boundary): shared length accessor under the valid-handle precondition
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_dependency_evaluation_count(
    assignment: Handle<ResolutionDetails>,
) -> usize {
    // SAFETY: the caller guarantees a valid handle.
    unsafe { assignment.as_ref() }.dependency_evaluations.len()
}

/// Get a borrowed prerequisite evaluation by index.
///
/// Returns `NULL` when `index` is out of bounds. The returned pointer is owned by `assignment`
/// and must not be freed or used after `assignment` is dropped.
///
/// # Safety
/// `assignment` must be a valid handle.
// allow(ffi-panic-boundary): checked index accessor under the valid-handle precondition
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_dependency_evaluation(
    assignment: Handle<ResolutionDetails>,
    index: usize,
) -> *const DependencyEvaluation {
    // SAFETY: the caller guarantees a valid handle.
    unsafe { assignment.as_ref() }
        .dependency_evaluations
        .get(index)
        .map_or(std::ptr::null(), std::ptr::from_ref)
}

/// Get the flag key for a borrowed prerequisite evaluation.
///
/// # Safety
/// A non-NULL `evaluation` must be borrowed from a live assignment handle.
// allow(ffi-panic-boundary): null-checked shared string accessor
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
/// A non-NULL `evaluation` must be borrowed from a live assignment handle.
// allow(ffi-panic-boundary): null-checked shared value accessor
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
/// A non-NULL `evaluation` must be borrowed from a live assignment handle.
// allow(ffi-panic-boundary): null-checked shared string accessor
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
/// A non-NULL `evaluation` must be borrowed from a live assignment handle.
// allow(ffi-panic-boundary): null-checked shared string accessor
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

/// Get the reason for a borrowed prerequisite evaluation.
///
/// # Safety
/// A non-NULL `evaluation` must be borrowed from a live assignment handle.
// allow(ffi-panic-boundary): null-checked enum mapping
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

/// Get the error code for a borrowed prerequisite evaluation.
///
/// # Safety
/// A non-NULL `evaluation` must be borrowed from a live assignment handle.
// allow(ffi-panic-boundary): null-checked enum mapping
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
/// A non-NULL `evaluation` must be borrowed from a live assignment handle.
// allow(ffi-panic-boundary): null-checked shared string accessor
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

/// Get the split serial ID for a borrowed prerequisite evaluation.
///
/// # Safety
/// A non-NULL `evaluation` must be borrowed from a live assignment handle.
// allow(ffi-panic-boundary): null-checked scalar accessor
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

/// Whether this prerequisite belongs to the successful root assignment path and its selected
/// assignment permits exposure logging. SDKs must still apply consent and ordinary exposure
/// deduplication policy.
///
/// # Safety
/// A non-NULL `evaluation` must be borrowed from a live assignment handle.
// allow(ffi-panic-boundary): null-checked scalar accessor
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
/// A non-NULL `evaluation` must be borrowed from a live assignment handle.
// allow(ffi-panic-boundary): null-checked shared slice accessor
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
    use std::{collections::HashMap, sync::Arc};

    use super::*;

    #[test]
    fn evaluation_panic_returns_owned_redacted_error() {
        let mut result = catch_assignment_panic(|| panic!("private context value"));
        // SAFETY: the returned handle is live until it is freed below.
        let details = unsafe { result.as_ref() };
        assert_eq!(ErrorCode::from(details), ErrorCode::General);
        assert_eq!(
            details.error_message.as_deref(),
            Some("internal error: feature flag evaluation failed")
        );
        assert!(details.flag_metadata.is_empty());
        // SAFETY: all borrows ended; free the owned result once.
        unsafe { ddog_ffe_assignment_drop(&mut result) };
    }

    #[test]
    fn opt_out_modes_cross_the_ffi_boundary() {
        let wire = ffe::UniversalFlagConfig::from_json(
            br#"{
                "createdAt":"2026-10-19T00:00:00Z", "environment":{"name":"test"},
                "flags":{"feature":{
                    "key":"feature", "enabled":true, "variationType":"BOOLEAN",
                    "variations":{"on":{"key":"on","value":true},"off":{"key":"off","value":false}},
                    "allocations":[
                        {"key":"experiment","doLog":true,"randomizationActivatedAt":"2026-10-19T12:00:00Z",
                         "splits":[{"variationKey":"on","shards":[]}]},
                        {"key":"ordinary","doLog":false,"splits":[{"variationKey":"off","shards":[]}]}
                    ]
                }}
            }"#.to_vec(),
        )
        .unwrap();
        let mut config = Handle::new(Configuration::from_server_response(wire));
        for (cutoff, future_excluded) in [
            ("2026-10-19T11:59:59.999999999Z", true),
            ("2026-10-19T12:00:00Z", true),
            ("2026-10-19T12:00:00.000000001Z", false),
        ] {
            let mut context = Handle::new(EvaluationContext::new(
                Some("subject".into()),
                Arc::new(HashMap::from([(
                    "dd_experimentation_opt_out_at".into(),
                    ffe::Attribute::from(cutoff),
                )])),
            ));
            for (mode, excluded) in [
                (ExperimentationOptOutMode::Disabled, false),
                (ExperimentationOptOutMode::Immediate, true),
                (
                    ExperimentationOptOutMode::FutureExperimentsOnly,
                    future_excluded,
                ),
            ] {
                // SAFETY: handles have no Drop. Copies borrow live resources, as C callers do.
                // Each original handle is freed exactly once after its last use.
                let config_copy = unsafe { std::ptr::read(&config) };
                // SAFETY: the context stays live throughout this evaluation.
                let context_copy = unsafe { std::ptr::read(&context) };
                // SAFETY: both handles are live and the flag key is a static C string.
                let mut result = unsafe {
                    ddog_ffe_get_assignment_with_opt_out_mode(
                        config_copy,
                        c"feature".as_ptr(),
                        ExpectedFlagType::Boolean,
                        context_copy,
                        mode,
                    )
                };
                // SAFETY: the returned handle is live until the drop below.
                let details = unsafe { result.as_ref() };
                assert_eq!(ErrorCode::from(details), ErrorCode::Ok);
                let assignment = details.as_ref().as_ref().unwrap();
                assert_eq!(assignment.do_log, !excluded);
                assert_eq!(
                    assignment.allocation_key.as_str(),
                    if excluded { "ordinary" } else { "experiment" }
                );
                // SAFETY: the result is no longer borrowed and is freed once.
                unsafe { ddog_ffe_assignment_drop(&mut result) };
            }
            // SAFETY: these copies borrow the live resources; only the originals are freed.
            let config_copy = unsafe { std::ptr::read(&config) };
            // SAFETY: the context stays live through the existing boolean interface call.
            let context_copy = unsafe { std::ptr::read(&context) };
            // SAFETY: both handles are live and the flag key is a static C string.
            let mut result = unsafe {
                ddog_ffe_get_assignment_with_options(
                    config_copy,
                    c"feature".as_ptr(),
                    ExpectedFlagType::Boolean,
                    context_copy,
                    EvaluationOptions {
                        experimentation_opt_out_immediate: true,
                    },
                )
            };
            // SAFETY: the result stays live until the drop below.
            let details = unsafe { result.as_ref() };
            assert_eq!(ErrorCode::from(details), ErrorCode::Ok);
            assert!(!details.as_ref().as_ref().unwrap().do_log);
            // SAFETY: all result borrows ended; free each handle exactly once.
            unsafe { ddog_ffe_assignment_drop(&mut result) };
            // SAFETY: all calls using the context completed.
            unsafe { Handle::free(&mut context) };
        }
        // SAFETY: all calls completed and the configuration handle is still live.
        unsafe { Handle::free(&mut config) };
    }

    #[test]
    fn rejected_flag_is_exposed_as_parse_error() {
        let details = ResolutionDetails::from(EvaluationError::FlagConfigurationInvalid);

        assert!(matches!(Reason::from(&details), Reason::Error));
        assert_eq!(ErrorCode::from(&details), ErrorCode::ParseError);
    }

    #[test]
    fn invalid_opt_out_is_exposed_as_invalid_context() {
        let details = ResolutionDetails::from(EvaluationError::InvalidContext);
        assert!(matches!(Reason::from(&details), Reason::Error));
        assert_eq!(ErrorCode::from(&details), ErrorCode::InvalidContext);
    }

    #[test]
    fn unassigned_dependency_is_an_error() {
        let dependency = DependencyEvaluation {
            flag_key: "parent".into(),
            details: EvaluationDetails::new(Err(EvaluationError::DefaultAllocationNull)),
            exposure_eligible: false,
        };
        let dependency = std::ptr::from_ref(&dependency);
        // SAFETY: the pointer refers to the live local dependency.
        let reason = unsafe { ddog_ffe_dependency_evaluation_get_reason(dependency) };
        assert!(matches!(reason, Reason::Error));
        // SAFETY: the pointer refers to the live local dependency.
        let error_code = unsafe { ddog_ffe_dependency_evaluation_get_error_code(dependency) };
        assert_eq!(error_code, ErrorCode::General);
        // SAFETY: the pointer refers to the live local dependency.
        let message = unsafe { ddog_ffe_dependency_evaluation_get_error_message(dependency) };
        // SAFETY: the borrowed static message is always live.
        let message = unsafe { message.as_bytes() };
        assert_eq!(message, b"prerequisite has no assignment");
    }

    #[test]
    fn dependency_batch_crosses_the_ffi_boundary() {
        let wire = ffe::UniversalFlagConfig::from_json(
            br#"{
                "createdAt":"2026-10-30T00:00:00Z", "environment":{"name":"test"},
                "flags":{
                    "root":{
                        "key":"root", "enabled":true, "variationType":"BOOLEAN",
                        "variations":{"on":{"key":"on","value":true}},
                        "allocations":[{
                            "key":"root-allocation", "doLog":true,
                            "rules":[{"conditions":[{
                                "flagEvaluation":{"key":"parent"},
                                "operator":"ONE_OF", "value":["on"]
                            }]}],
                            "splits":[{"variationKey":"on","shards":[]}]
                        }]
                    },
                    "parent":{
                        "key":"parent", "enabled":true, "variationType":"BOOLEAN",
                        "variations":{"on":{"key":"on","value":true}},
                        "allocations":[{
                            "key":"parent-allocation", "doLog":false,
                            "splits":[{"variationKey":"on","shards":[]}]
                        }]
                    }
                }
            }"#
            .to_vec(),
        )
        .unwrap();
        let mut config = Handle::new(Configuration::from_server_response(wire));
        let mut context = Handle::new(EvaluationContext::new(
            Some("subject".into()),
            Default::default(),
        ));
        // SAFETY: these copies borrow the live handles; only the originals are freed below.
        let config_copy = unsafe { std::ptr::read(&config) };
        // SAFETY: the context remains live through the call.
        let context_copy = unsafe { std::ptr::read(&context) };
        // SAFETY: both handles and the static C string are valid.
        let mut result = unsafe {
            ddog_ffe_get_assignment(
                config_copy,
                c"root".as_ptr(),
                ExpectedFlagType::Boolean,
                context_copy,
            )
        };

        // SAFETY: this copy borrows the live result handle.
        let result_copy = unsafe { std::ptr::read(&result) };
        // SAFETY: the copied handle remains live until the original is freed below.
        let dependency_count =
            unsafe { ddog_ffe_assignment_get_dependency_evaluation_count(result_copy) };
        assert_eq!(dependency_count, 1);
        // SAFETY: this copy borrows the live result handle.
        let result_copy = unsafe { std::ptr::read(&result) };
        // SAFETY: the copied handle remains live and index zero exists.
        let dependency = unsafe { ddog_ffe_assignment_get_dependency_evaluation(result_copy, 0) };
        assert!(!dependency.is_null());
        // SAFETY: `dependency` is borrowed from the still-live result.
        let key = unsafe { ddog_ffe_dependency_evaluation_get_flag_key(dependency) };
        // SAFETY: the borrowed key remains valid while `result` is live.
        assert_eq!(unsafe { key.as_bytes() }, b"parent");
        // SAFETY: `dependency` is borrowed from the still-live result.
        assert!(!unsafe { ddog_ffe_dependency_evaluation_get_exposure_eligible(dependency) });
        // SAFETY: `dependency` is borrowed from the still-live result.
        let error_code = unsafe { ddog_ffe_dependency_evaluation_get_error_code(dependency) };
        assert_eq!(error_code, ErrorCode::Ok);
        // SAFETY: this copy borrows the live result handle.
        let result_copy = unsafe { std::ptr::read(&result) };
        // SAFETY: the handle remains live; the checked accessor returns NULL out of bounds.
        let out_of_bounds =
            unsafe { ddog_ffe_assignment_get_dependency_evaluation(result_copy, 1) };
        assert!(out_of_bounds.is_null());

        // SAFETY: all result borrows ended; free the owned result exactly once.
        unsafe { ddog_ffe_assignment_drop(&mut result) };
        // SAFETY: all context borrows ended; free the owned context exactly once.
        unsafe { Handle::free(&mut context) };
        // SAFETY: all configuration borrows ended; free the owned configuration exactly once.
        unsafe { Handle::free(&mut config) };
    }
}
