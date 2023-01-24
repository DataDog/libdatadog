extern crate prost_build;
extern crate napi_build;

fn main() {
    napi_build::setup();

    prost_build::compile_protos(&["./src/agent_payload.proto"],
                                &["src/"]).unwrap();
}
