// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::borrow::Cow;
use datadog_live_debugger::{DslString, ProbeCondition};
use ddcommon_ffi::CharSlice;
use std::ffi::c_void;
use std::mem::transmute;

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
        for<'a, 'b> extern "C" fn(&'a mut c_void, &CharSlice<'b>) -> Option<&'a c_void>, // special values: @duration, @return, @exception
    pub fetch_index: for<'a, 'b> extern "C" fn(
        &'a mut c_void,
        &'a c_void,
        IntermediateValue<'b>,
    ) -> Option<&'a c_void>,
    pub fetch_nested: for<'a, 'b> extern "C" fn(
        &'a mut c_void,
        &'a c_void,
        IntermediateValue<'b>,
    ) -> Option<&'a c_void>,
    pub length: for<'a> extern "C" fn(&'a mut c_void, &'a c_void) -> u64,
    pub try_enumerate: for<'a> extern "C" fn(&'a mut c_void, &'a c_void) -> VoidCollection,
    pub stringify: for<'a> extern "C" fn(&'a mut c_void, &'a c_void) -> VoidCollection,
    pub convert_index: for<'a> extern "C" fn(&'a mut c_void, &'a c_void) -> isize, // return < 0 on error
}

static mut FFI_EVALUATOR: Option<Evaluator> = None;
#[allow(mutable_transmutes)] // SAFETY: It's the &mut c_void context we receive from input functions
static EVALUATOR: datadog_live_debugger::Evaluator<c_void, c_void> =
    datadog_live_debugger::Evaluator {
        equals: |context, a, b| unsafe {
            (FFI_EVALUATOR.as_ref().unwrap().equals)(transmute(context), (&a).into(), (&b).into())
        },
        greater_than: |context, a, b| unsafe {
            (FFI_EVALUATOR.as_ref().unwrap().greater_than)(transmute(context), (&a).into(), (&b).into())
        },
        greater_or_equals: |context, a, b| unsafe {
            (FFI_EVALUATOR.as_ref().unwrap().greater_or_equals)(transmute(context), (&a).into(), (&b).into())
        },
        fetch_identifier: |context, name| unsafe {
            (FFI_EVALUATOR.as_ref().unwrap().fetch_identifier)(transmute(context), &CharSlice::from(name))
        },
        fetch_index: |context, base, index| unsafe {
            (FFI_EVALUATOR.as_ref().unwrap().fetch_index)(transmute(context), base, (&index).into())
        },
        fetch_nested: |context, base, member| unsafe {
            (FFI_EVALUATOR.as_ref().unwrap().fetch_nested)(transmute(context), base, (&member).into())
        },
        length: |context, value| unsafe {
            (FFI_EVALUATOR.as_ref().unwrap().length)(transmute(context), value)
        },
        try_enumerate: |context, value| unsafe {
            let collection = (FFI_EVALUATOR.as_ref().unwrap().try_enumerate)(transmute(context), value);
            if collection.count < 0 {
                None
            } else {
                // We need to copy, Vec::from_raw_parts with only free in the allocator would be unstable...
                let mut vec = Vec::with_capacity(collection.count as usize);
                vec.extend_from_slice(std::slice::from_raw_parts(
                    collection.elements as *const &c_void,
                    collection.count as usize,
                ));
                (collection.free)(collection);
                Some(vec)
            }
        },
        stringify: |context, value| unsafe {
            let collection = (FFI_EVALUATOR.as_ref().unwrap().try_enumerate)(transmute(context), value);
            if collection.count < 0 {
                unreachable!()
            }

            // We need to copy...
            let string = String::from_raw_parts(
                collection.elements as *mut u8,
                collection.count as usize,
                collection.count as usize,
            );
            let copy = string.clone();
            std::mem::forget(string);
            (collection.free)(collection);
            Cow::Owned(copy)
        },
        convert_index: |context, value| unsafe {
            let index = (FFI_EVALUATOR.as_ref().unwrap().convert_index)(transmute(context), value);
            if index < 0 {
                None
            } else {
                Some(index as usize)
            }
        },
    };

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn register_expr_evaluator(eval: &Evaluator) {
    FFI_EVALUATOR = Some(eval.clone());
}

#[no_mangle]
pub extern "C" fn evaluate_condition(condition: &ProbeCondition, context: &mut c_void) -> bool {
    datadog_live_debugger::eval_condition(&EVALUATOR, condition, context)
}

pub fn evaluate_string(condition: &DslString, context: &mut c_void) -> String {
    datadog_live_debugger::eval_string(&EVALUATOR, condition, context)
}

// This is unsafe, but we want to use it as function pointer...
#[no_mangle]
extern "C" fn drop_void_collection_string(void: VoidCollection) {
    unsafe {
        String::from_raw_parts(
            void.elements as *mut u8,
            void.count as usize,
            void.count as usize,
        );
    }
}

#[no_mangle]
pub extern "C" fn evaluate_unmanaged_string(
    condition: &DslString,
    context: &mut c_void,
) -> VoidCollection {
    let string = evaluate_string(condition, context);
    let new = VoidCollection {
        count: string.len() as isize,
        elements: string.as_ptr() as *const c_void,
        free: drop_void_collection_string as extern "C" fn(VoidCollection),
    };
    std::mem::forget(string);
    new
}
