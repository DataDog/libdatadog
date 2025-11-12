// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_char, CStr};

use datadog_ffe::rules_based as ffe;
use datadog_ffe::rules_based::{
    now, Assignment, AssignmentReason, AssignmentValue, Configuration, EvaluationContext,
    EvaluationError, Str,
};

use crate::Handle;

/// Opaque type representing a result of evaluation.
pub struct ResolutionDetails {
    inner: Result<Assignment, EvaluationError>,
    // memoizing some fields, so we can hand off references to them:
    error_message: Option<String>,
    extra_logging: Vec<KeyValue<BorrowedStr, BorrowedStr>>,
    flag_metadata: Vec<KeyValue<BorrowedStr, BorrowedStr>>,
}
impl ResolutionDetails {
    fn new(value: Result<Assignment, EvaluationError>) -> ResolutionDetails {
        let error_message = value.as_ref().err().map(|err| err.to_string());

        let extra_logging = value
            .as_ref()
            .iter()
            .flat_map(|it| it.extra_logging.iter())
            .map(|(k, v)| {
                KeyValue {
                    // SAFETY: the borrow is valid as long as string allocation is
                    // alive. ResolutionDetails will get moved into heap but this does not
                    // invalidate the string.
                    key: unsafe { BorrowedStr::borrow_from_str(k.as_str()) },
                    // SAFETY: the borrow is valid as long as string allocation is
                    // alive. ResolutionDetails will get moved into heap but this does not
                    // innvalidate the string.
                    value: unsafe { BorrowedStr::borrow_from_str(v.as_str()) },
                }
            })
            .collect();

        let flag_metadata = match value.as_ref() {
            Ok(a) => {
                vec![KeyValue {
                    // SAFETY: borrowing from static is safe as it lives long enough.
                    key: unsafe { BorrowedStr::borrow_from_str("allocation_key") },
                    // SAFETY: allocation_key is alive until ResolutionDetails is dropped.
                    value: unsafe { BorrowedStr::borrow_from_str(a.allocation_key.as_str()) },
                }]
            }
            Err(_) => Vec::new(),
        };

        ResolutionDetails {
            inner: value,
            error_message,
            extra_logging,
            flag_metadata,
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
#[no_mangle]
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

    let assignment_result = config.eval_flag(flag_key, context, expected_type.into(), now());

    Handle::new(assignment_result.into())
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
#[no_mangle]
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
#[no_mangle]
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
#[no_mangle]
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
#[no_mangle]
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
            AssignmentReason::Static => Reason::Static,
        }
    }
}

/// # Safety
/// `assignment` must be a valid handle.
#[no_mangle]
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
#[no_mangle]
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

/// # Safety
/// `assignment` must be a valid handle.
#[no_mangle]
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
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_assignnment_get_flag_metadata(
    assignment: Handle<ResolutionDetails>,
) -> ArrayMap<BorrowedStr, BorrowedStr> {
    // SAFETY: the caller must ensure that assignment is valid
    let a = unsafe { assignment.as_ref() };
    // SAFETY: the caller must ensure that returned value is not used after `assignment` is freed.
    unsafe { ArrayMap::borrow_from_slice(&a.flag_metadata) }
}

/// # Safety
/// `assignment` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_assignnment_get_extra_logging(
    assignment: Handle<ResolutionDetails>,
) -> ArrayMap<BorrowedStr, BorrowedStr> {
    // SAFETY: the caller must ensure that assignment is valid
    let a = unsafe { assignment.as_ref() };
    // SAFETY: the caller must ensure that returned value is not used after `assignment` is freed.
    unsafe { ArrayMap::borrow_from_slice(&a.extra_logging) }
}

/// Frees an Assignment handle.
///
/// # Safety
/// - `assignment` must be a valid Assignment handle
#[no_mangle]
pub unsafe extern "C" fn ddog_ffe_assignment_drop(assignment: *mut Handle<ResolutionDetails>) {
    // SAFETY: the caller must ensure that assignment is valid
    unsafe { Handle::free(assignment) }
}
