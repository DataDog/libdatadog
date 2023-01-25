extern crate napi_build;
extern crate prost_build;

fn main() {
    napi_build::setup();

    prost_build::compile_protos(&["./src/agent_payload.proto"], &["src/"]).unwrap();
}
