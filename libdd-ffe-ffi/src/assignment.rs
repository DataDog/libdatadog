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
    // Views of the selected split's features, borrowing from the pinned assignment. Empty (and
    // unallocated) when the split has no features.
    features: Vec<Feature>,
    // Backing storage for every feature's `destinations` array, laid out contiguously so each
    // feature can borrow a (ptr, count) slice into it.
    // Read only through the raw pointers stored in `features`; must not be dropped or shrunk
    // while the handle lives.
    #[allow(dead_code)]
    feature_destinations: Vec<BorrowedStr>,
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

        let (features, feature_destinations) = Self::borrow_features((*inner).as_ref());

        ResolutionDetails {
            inner,
            error_message,
            flag_metadata,
            features,
            feature_destinations,
        }
    }

    fn borrow_features(
        result: Result<&Assignment, &EvaluationError>,
    ) -> (Vec<Feature>, Vec<BorrowedStr>) {
        let Ok(assignment) = result else {
            return (Vec::new(), Vec::new());
        };
        if assignment.features.is_empty() {
            return (Vec::new(), Vec::new());
        }

        let total_destinations: usize = assignment
            .features
            .iter()
            .map(|feature| feature.destinations.len())
            .sum();
        // SAFETY: the same borrow-from-pinned-assignment discipline as `flag_metadata` above.
        // All destination strings are borrowed up front, so the pointers handed out below stay
        // valid: the Vec is never reallocated after this loop.
        let mut feature_destinations = Vec::with_capacity(total_destinations);
        for feature in assignment.features.iter() {
            for destination in feature.destinations.iter() {
                // SAFETY: borrows from the pinned assignment, which ResolutionDetails owns.
                feature_destinations
                    .push(unsafe { BorrowedStr::borrow_from_str(destination.as_str()) });
            }
        }

        let mut offset = 0;
        let features = assignment
            .features
            .iter()
            .map(|feature| {
                let count = feature.destinations.len();
                let destinations = BorrowedStrArray {
                    elements: if count == 0 {
                        std::ptr::null()
                    } else {
                        feature_destinations[offset..offset + count].as_ptr()
                    },
                    count,
                };
                offset += count;
                Feature {
                    // SAFETY: see above.
                    key: unsafe { BorrowedStr::borrow_from_str(feature.key.as_str()) },
                    value: match &feature.value {
                        libdd_ffe::rules_based::FeatureValue::String(s) => {
                            // SAFETY: borrows from the pinned assignment, which ResolutionDetails
                            // owns.
                            FeatureValue::String(unsafe {
                                BorrowedStr::borrow_from_str(s.as_str())
                            })
                        }
                        libdd_ffe::rules_based::FeatureValue::Number(n) => FeatureValue::Float(*n),
                        libdd_ffe::rules_based::FeatureValue::Boolean(b) => {
                            FeatureValue::Boolean(*b)
                        }
                    },
                    destinations,
                }
            })
            .collect();

        (features, feature_destinations)
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

    let assignment_result = config.eval_flag_with_options(
        flag_key,
        context,
        expected_type.into(),
        now(),
        ffe::EvaluationOptions {
            experimentation_opt_out_mode: mode.into(),
        },
    );

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

/// Value of a feature attached to the selected split.
#[repr(C)]
pub enum FeatureValue {
    /// The string is not NUL-terminated; see `BorrowedStr`.
    String(BorrowedStr),
    Float(f64),
    Boolean(bool),
}

/// A borrowed array of `BorrowedStr`. `elements` is NULL when `count` is `0`.
#[repr(C)]
pub struct BorrowedStrArray {
    pub elements: *const BorrowedStr,
    pub count: usize,
}

/// A feature attached to the selected split.
#[repr(C)]
pub struct Feature {
    pub key: BorrowedStr,
    pub value: FeatureValue,
    pub destinations: BorrowedStrArray,
}

/// A borrowed array of `Feature`. `elements` is NULL when `count` is `0`.
#[repr(C)]
pub struct FeatureArray {
    pub elements: *const Feature,
    pub count: usize,
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

/// Get the features attached to the selected split.
///
/// Entries with unsupported value types, empty keys or no non-empty destinations are absent
/// from the array. On a repeated key only the first valid feature is present. Destinations pass
/// through unfiltered; the caller decides what to do with unknown values.
///
/// # Ownership
///
/// The returned array borrows from `assignment`. It must not be used after `assignment` is
/// freed.
///
/// # Safety
/// `assignment` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ffe_assignment_get_features(
    assignment: Handle<ResolutionDetails>,
) -> FeatureArray {
    // SAFETY: the caller must ensure that assignment is valid
    let a = unsafe { assignment.as_ref() };
    if a.features.is_empty() {
        FeatureArray {
            elements: std::ptr::null(),
            count: 0,
        }
    } else {
        // SAFETY: the caller must not use returned value after `assignment` is freed.
        FeatureArray {
            elements: a.features.as_ptr(),
            count: a.features.len(),
        }
    }
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
    use libdd_ffe::rules_based::{AssignmentReason, Feature as FfeFeature};

    fn assignment_with_features(features: Vec<FfeFeature>) -> ResolutionDetails {
        ResolutionDetails::from(Assignment {
            value: AssignmentValue::String(Str::from("on")),
            variation_key: Str::from("on"),
            allocation_key: Str::from("static"),
            reason: AssignmentReason::Static,
            serial_id: Some(7),
            do_log: true,
            features: features.into(),
        })
    }

    fn ffe_feature(
        key: &str,
        value: libdd_ffe::rules_based::FeatureValue,
        destinations: &[&str],
    ) -> FfeFeature {
        FfeFeature {
            key: Str::from(key),
            value,
            destinations: destinations.iter().map(|d| Str::from(*d)).collect(),
        }
    }

    fn borrowed(s: &BorrowedStr) -> &str {
        // SAFETY: test-only view over a BorrowedStr constructed from a valid &str.
        std::str::from_utf8(unsafe { s.as_bytes() }).unwrap()
    }

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
    fn features_are_borrowed_from_the_assignment() {
        let details = assignment_with_features(vec![
            ffe_feature(
                "holdout.key",
                libdd_ffe::rules_based::FeatureValue::String(Str::from("abc")),
                &["HOOK"],
            ),
            ffe_feature(
                "holdout.fraction",
                libdd_ffe::rules_based::FeatureValue::Number(0.5),
                &["HOOK", "SOME_NEW_DESTINATION"],
            ),
            ffe_feature(
                "holdout.enabled",
                libdd_ffe::rules_based::FeatureValue::Boolean(false),
                &["EXPOSURE"],
            ),
        ]);

        assert_eq!(details.features.len(), 3);
        assert_eq!(borrowed(&details.features[0].key), "holdout.key");
        match &details.features[0].value {
            FeatureValue::String(s) => assert_eq!(borrowed(s), "abc"),
            _ => panic!("expected string"),
        }
        match &details.features[1].value {
            FeatureValue::Float(f) => assert_eq!(*f, 0.5),
            _ => panic!("expected float"),
        }
        match &details.features[2].value {
            FeatureValue::Boolean(b) => assert!(!*b),
            _ => panic!("expected boolean"),
        }

        for (feature, expected) in details.features.iter().zip([
            &["HOOK"][..],
            &["HOOK", "SOME_NEW_DESTINATION"][..],
            &["EXPOSURE"][..],
        ]) {
            // SAFETY: the pointer and count come from borrow_features over a live handle.
            let destinations = unsafe {
                std::slice::from_raw_parts(
                    feature.destinations.elements,
                    feature.destinations.count,
                )
            };
            assert_eq!(destinations.len(), expected.len());
            for (destination, expected) in destinations.iter().zip(expected) {
                assert_eq!(borrowed(destination), *expected);
            }
        }
    }

    #[test]
    fn assignment_without_features_exposes_an_empty_array() {
        let details = assignment_with_features(vec![]);

        assert!(details.features.is_empty());
        assert!(details.feature_destinations.is_empty());

        let handle = Handle::new(details);
        // SAFETY: Handle is a plain pointer wrapper; a bitwise copy is sound.
        let handle_copy = unsafe { std::ptr::read(&handle) };
        // SAFETY: the handle is valid; the original is freed below.
        let features = unsafe { ddog_ffe_assignment_get_features(handle_copy) };
        assert_eq!(features.count, 0);
        assert!(features.elements.is_null());

        // SAFETY: freeing the test handle.
        unsafe { ddog_ffe_assignment_drop(Box::into_raw(Box::new(handle))) };
    }

    #[test]
    fn error_result_exposes_an_empty_array() {
        let handle = Handle::new(ResolutionDetails::from(
            EvaluationError::ConfigurationMissing,
        ));
        // SAFETY: Handle is a plain pointer wrapper; a bitwise copy is sound.
        let handle_copy = unsafe { std::ptr::read(&handle) };
        // SAFETY: the handle is valid; the original is freed below.
        let features = unsafe { ddog_ffe_assignment_get_features(handle_copy) };
        assert_eq!(features.count, 0);
        assert!(features.elements.is_null());

        // SAFETY: freeing the test handle.
        unsafe { ddog_ffe_assignment_drop(Box::into_raw(Box::new(handle))) };
    }
}
