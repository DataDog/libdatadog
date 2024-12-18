use std::path::PathBuf;

use ddcommon_ffi as ffi;

#[repr(C)]
pub struct ProcessInfo<'a> {
    pub args: ffi::Slice<'a, ffi::CharSlice<'a>>,
    pub envp: ffi::Slice<'a, ffi::CharSlice<'a>>,
    language: ffi::CharSlice<'a>,
}

#[repr(C)]
pub enum Value {
    NumVal(i64),
    BoolVal(bool),
    StrVal(ffi::StringWrapper),
}

#[repr(C)]
pub enum ConfigName {
    DdTraceDebug = 0,
}

#[repr(C)]
pub struct Config {
    pub name: ConfigName,
    pub value: Value,
}

#[derive(Debug)]
pub struct Configurator {
    debug_logs: bool,
    #[allow(dead_code)]
    static_config_file_path: PathBuf,
}

#[no_mangle]
pub extern "C" fn ddog_library_config_new(debug_logs: bool) -> Box<Configurator> {
    Box::new(Configurator {
        debug_logs,
        static_config_file_path: PathBuf::from(
            "/etc/datadog-agent/managed/datadog-apm-libraries/st",
        ),
    })
}

#[no_mangle]
pub extern "C" fn ddog_library_config_drop(_: Box<Configurator>) {}

#[no_mangle]
pub extern "C" fn ddog_library_config_get<'a>(
    configurator: &'a Configurator,
    process_info: ProcessInfo<'a>,
) -> ffi::Vec<Config> {
    if configurator.debug_logs {
        println!("Called library_config_common_component:");
        println!("\tconfigurator: {:?}", configurator);
        println!("\tprocess args: {:?}", process_info.args);
        // TODO: this is for testing purpose, we don't want to log env variables
        println!("\tprocess envs: {:?}", process_info.args);
    }
    ffi::Vec::from(vec![Config {
        name: ConfigName::DdTraceDebug,
        value: Value::BoolVal(true),
    }])
}
