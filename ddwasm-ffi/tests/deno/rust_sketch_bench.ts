import init, {read_container_id, Sketch} from "../../pkg/ddwasm_ffi.js";
await Deno.readFile('./pkg/ddwasm_ffi_bg.wasm');
await init(Deno.readFile('./pkg/ddwasm_ffi_bg.wasm'));
console.log("Container ID:", await read_container_id());

Deno.bench("Read container id", async () => {
    await read_container_id();
})

const rust_sketch = new Sketch();
Deno.bench("sketches_ddsketch::add", () => {
    rust_sketch.add(1);
});

import {DDSketch} from "https://esm.sh/@datadog/sketches-js@1.0.4";

const ts_sketch = new DDSketch();
Deno.bench("Datadog/sketch-js accept", () => {
    ts_sketch.accept(1);
});

Deno.bench("Datadog/sketch-js toProto", () => {
    ts_sketch.toProto();
});

