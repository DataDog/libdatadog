// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::borrow::Cow;
use datadog_live_debugger::{DslString, ProbeCondition};
use ddcommon_ffi::CharSlice;
use std::ffi::c_void;
use ddcommon_ffi::slice::AsBytes;
use datadog_live_debugger::debugger_defs::SnapshotEvaluationError;

#[repr(C)]
pub enum IntermediateValue<'a> {
    String(CharSlice<'a>),
    Number(f64),
    Bool(bool),
    Null,
    Referenced(&'a c_void),
}

impl<'a> From<&'a datadog_live_debugger::IntermediateValue<'a, c_void>> for IntermediateValue<'a> {
    fn from(value: &'a datadog_live_debugger::IntermediateValue<'a, c_void>) -> Self {
        match value {
            datadog_live_debugger::IntermediateValue::String(s) => {
                IntermediateValue::String(s.as_ref().into())
            }
            datadog_live_debugger::IntermediateValue::Number(n) => IntermediateValue::Number(*n),
            datadog_live_debugger::IntermediateValue::Bool(b) => IntermediateValue::Bool(*b),
            datadog_live_debugger::IntermediateValue::Null => IntermediateValue::Null,
            datadog_live_debugger::IntermediateValue::Referenced(value) => {
                IntermediateValue::Referenced(value)
            }
        }
    }
}

#[repr(C)]
pub struct VoidCollection {
    pub count: isize, // set to < 0 on error
    pub elements: *const c_void,
    pub free: extern "C" fn(VoidCollection),
}

#[repr(C)]
#[derive(Clone)]
pub struct Evaluator {
    pub equals:
        for<'a> extern "C" fn(&'a mut c_void, IntermediateValue<'a>, IntermediateValue<'a>) -> bool,
    pub greater_than:
        for<'a> extern "C" fn(&'a mut c_void, IntermediateValue<'a>, IntermediateValue<'a>) -> bool,
    pub greater_or_equals:
        for<'a> extern "C" fn(&'a mut c_void, IntermediateValue<'a>, IntermediateValue<'a>) -> bool,
    pub fetch_identifier:
        for<'a, 'b> extern "C" fn(&'a mut c_void, &CharSlice<'b>) -> Option<&'static c_void>, // special values: @duration, @return, @exception
    pub fetch_index: for<'a, 'b> extern "C" fn(
        &'a mut c_void,
        &'a c_void,
        IntermediateValue<'b>,
    ) -> Option<&'static c_void>,
    pub fetch_nested: for<'a, 'b> extern "C" fn(
        &'a mut c_void,
        &'a c_void,
        IntermediateValue<'b>,
    ) -> Option<&'static c_void>,
    pub length: for<'a> extern "C" fn(&'a mut c_void, &'a c_void) -> usize,
    pub try_enumerate: for<'a> extern "C" fn(&'a mut c_void, &'a c_void) -> VoidCollection,
    pub stringify: for<'a> extern "C" fn(&'a mut c_void, &'a c_void) -> CharSlice<'static>,
    pub get_string: for<'a> extern "C" fn(&'a mut c_void, &'a c_void) -> CharSlice<'static>,
    pub convert_index: for<'a> extern "C" fn(&'a mut c_void, &'a c_void) -> isize, // return < 0 on error
    pub instanceof: for<'a> extern "C" fn(&'a mut c_void, &'a c_void, &CharSlice<'a>) -> bool,
}

static mut FFI_EVALUATOR: Option<Evaluator> = None;

struct EvalCtx<'e> {
    context: &'e mut c_void,
    eval: &'static Evaluator,
}

impl<'e> EvalCtx<'e> {
    fn new(context: &'e mut c_void) -> Self {
        EvalCtx {
            context,
            eval: unsafe { FFI_EVALUATOR.as_ref().unwrap() }
        }
    }
}

impl<'e> datadog_live_debugger::Evaluator<'e, c_void> for EvalCtx<'e> {
    fn equals(&mut self, a: datadog_live_debugger::IntermediateValue<'e, c_void>, b: datadog_live_debugger::IntermediateValue<'e, c_void>) -> bool {
        (self.eval.equals)(self.context, (&a).into(), (&b).into())
    }

    fn greater_than(&mut self, a: datadog_live_debugger::IntermediateValue<'e, c_void>, b: datadog_live_debugger::IntermediateValue<'e, c_void>) -> bool {
        (self.eval.greater_than)(self.context, (&a).into(), (&b).into())
    }

    fn greater_or_equals(&mut self, a: datadog_live_debugger::IntermediateValue<'e, c_void>, b: datadog_live_debugger::IntermediateValue<'e, c_void>) -> bool {
        (self.eval.greater_or_equals)(self.context, (&a).into(), (&b).into())
    }

    fn fetch_identifier(&mut self, identifier: &str) -> Option<&'e c_void> {
        (self.eval.fetch_identifier)(self.context, &CharSlice::from(identifier))
    }

    fn fetch_index(&mut self, value: &'e c_void, index: datadog_live_debugger::IntermediateValue<'e, c_void>) -> Option<&'e c_void> {
        (self.eval.fetch_index)(self.context, value, (&index).into())
    }

    fn fetch_nested(&mut self, value: &'e c_void, member: datadog_live_debugger::IntermediateValue<'e, c_void>) -> Option<&'e c_void> {
        (self.eval.fetch_nested)(self.context, value, (&member).into())
    }

    fn length(&mut self, value: &'e c_void) -> usize {
        (self.eval.length)(self.context, value)
    }

    fn try_enumerate(&mut self, value: &'e c_void) -> Option<Vec<&'e c_void>> {
        let collection = (self.eval.try_enumerate)(self.context, value);
        if collection.count < 0 {
            None
        } else {
            // We need to copy, Vec::from_raw_parts with only free in the allocator would be unstable...
            let mut vec = Vec::with_capacity(collection.count as usize);
            unsafe { vec.extend_from_slice(std::slice::from_raw_parts(
                collection.elements as *const &c_void,
                collection.count as usize,
            )) };
            (collection.free)(collection);
            Some(vec)
        }
    }

    fn stringify(&mut self, value: &'e c_void) -> Cow<'e, str> {
        (self.eval.stringify)(self.context, value).to_utf8_lossy()
    }

    fn get_string(&mut self, value: &'e c_void) -> Cow<'e, str> {
        (self.eval.get_string)(self.context, value).to_utf8_lossy()
    }

    fn convert_index(&mut self, value: &'e c_void) -> Option<usize> {
        let index = (self.eval.convert_index)(self.context, value);
        if index < 0 {
            None
        } else {
            Some(index as usize)
        }
    }

    fn instanceof(&mut self, value: &'e c_void, class: &'e str) -> bool {
        (self.eval.instanceof)(self.context, value, &class.into())
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_register_expr_evaluator(eval: &Evaluator) {
    FFI_EVALUATOR = Some(eval.clone());
}

#[repr(C)]
pub enum ConditionEvaluationResult {
    Success,
    Failure,
    Error(Box<Vec<SnapshotEvaluationError>>)
}

#[no_mangle]
pub extern "C" fn ddog_evaluate_condition(condition: &ProbeCondition, context: &mut c_void) -> ConditionEvaluationResult {
    let mut ctx = EvalCtx::new(context);
    match datadog_live_debugger::eval_condition(&mut ctx, condition) {
        Ok(true) => ConditionEvaluationResult::Success,
        Ok(false) => ConditionEvaluationResult::Failure,
        Err(error) => ConditionEvaluationResult::Error(Box::new(vec![error]))
    }
}

pub fn ddog_evaluate_string<'a>(condition: &'a DslString, context: &'a mut c_void, errors: &mut Option<Box<Vec<SnapshotEvaluationError>>>) -> Cow<'a, str> {
    let mut ctx = EvalCtx::new(context);
    let (result, new_errors) = datadog_live_debugger::eval_string(&mut ctx, condition);
    if !new_errors.is_empty() {
        *errors = Some(Box::new(new_errors));
    }
    result
}

// This is unsafe, but we want to use it as function pointer...
#[no_mangle]
extern "C" fn ddog_drop_void_collection_string(void: VoidCollection) {
    unsafe {
        String::from_raw_parts(
            void.elements as *mut u8,
            void.count as usize,
            void.count as usize,
        );
    }
}

#[no_mangle]
pub extern "C" fn ddog_evaluate_unmanaged_string(
    segments: &DslString,
    context: &mut c_void,
    errors: &mut Option<Box<Vec<SnapshotEvaluationError>>>,
) -> VoidCollection {
    let string = ddog_evaluate_string(segments, context, errors).to_string();
    let new = VoidCollection {
        count: string.len() as isize,
        elements: string.as_ptr() as *const c_void,
        free: ddog_drop_void_collection_string as extern "C" fn(VoidCollection),
    };
    std::mem::forget(string);
    new
}
