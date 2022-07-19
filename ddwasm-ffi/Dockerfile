FROM denoland/deno:1.23.0
COPY pkg pkg
COPY tests tests

RUN deno bench --unstable --allow-read tests/deno/rust_sketch_bench.ts
