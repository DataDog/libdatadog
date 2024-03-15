// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use data_pipeline::trace_exporter::TraceExporter;
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jboolean, jint, jlong, jstring, JNI_TRUE};
use jni::JNIEnv;

#[no_mangle]
pub extern "system" fn Java_datadog_data_1pipeline_TraceExporter_create<'local>(
    mut env: JNIEnv,
    _class: JClass<'local>,
    host: JString<'local>,
    port: jint,
    tracer_version: JString<'local>,
    lang: JString<'local>,
    lang_version: JString<'local>,
    lang_interpreter: JString<'local>,
    proxy: jboolean,
) -> jlong {
    let mut builder = TraceExporter::builder();
    let exporter = builder
        .set_host(&get_string(&mut env, &host))
        .set_port(port as u16)
        .set_tracer_version(&get_string(&mut env, &tracer_version))
        .set_language(&get_string(&mut env, &lang))
        .set_language_version(&get_string(&mut env, &lang_version))
        .set_language_interpreter(&get_string(&mut env, &lang_interpreter))
        .set_proxy(proxy == JNI_TRUE)
        .build()
        .unwrap();

    Box::into_raw(Box::new(exporter)) as jlong
}

#[no_mangle]
pub extern "system" fn Java_datadog_data_1pipeline_TraceExporter_destroy<'local>(
    _env: JNIEnv,
    _class: JClass<'local>,
    exporter: jlong,
) {
    if exporter != 0 {
        let exporter = unsafe { Box::from_raw(exporter as *mut TraceExporter) };
        drop(exporter);
    } else {
    }
}

#[no_mangle]
pub extern "system" fn Java_datadog_data_1pipeline_TraceExporter_sendTraces<'local>(
    env: JNIEnv,
    _class: JClass<'local>,
    exporter: jlong,
    traces: JObject<'local>,
    traces_length: jint,
    traces_count: jint,
) -> jstring {
    let exporter = unsafe { Box::from_raw(exporter as *mut TraceExporter) };
    let traces_ptr = env.get_direct_buffer_address(&traces.into()).unwrap();
    let traces_slice = unsafe { std::slice::from_raw_parts(traces_ptr, traces_length as usize) };
    let res = exporter
        .send(traces_slice, traces_count as usize)
        .unwrap_or_else(|_| String::from(""));
    Box::leak(exporter);
    **env.new_string(&res).unwrap()
}

fn get_string(env: &mut JNIEnv, jstring: &JString) -> String {
    env.get_string(jstring).unwrap().into()
}
