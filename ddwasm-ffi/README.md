Quick and dirty PoC of Rust shared lib compiling into WASM
Uses Deno, because... I'm scared of package.json ;) 

To run the example benchmark + Container ID POC:

```
wasm-pack build --release --target web
docker build .
```

During docker build the benchmark/ example code will run.


Example output
```
Check file:///tests/deno/rust_sketch_bench.ts
Container ID: d982ec19608bed369e9673fdffde70e939b09e7ab040a1cbb2ac46b6911d8416
cpu: AMD Ryzen 5 4600H with Radeon Graphics
runtime: deno 1.23.0 (x86_64-unknown-linux-gnu)

file:///tests/deno/rust_sketch_bench.ts
benchmark                      time (avg)             (min … max)       p75       p99      p995
----------------------------------------------------------------- -----------------------------
Read container id          119.95 µs/iter     (76.82 µs … 3.8 ms) 121.41 µs 340.22 µs  411.9 µs
sketches_ddsketch::add      28.65 ns/iter   (26.12 ns … 79.38 ns)  28.22 ns  43.85 ns  62.11 ns
Datadog/sketch-js accept    11.77 ns/iter   (11.24 ns … 50.01 ns)  11.43 ns  22.65 ns  25.91 ns
Datadog/sketch-js toProto     5.5 µs/iter   (3.84 µs … 828.19 µs)   4.83 µs   23.8 µs  32.26 µs
Removing intermediate container d982ec19608b
 ---> a2ae5328fbce
Successfully built a2ae5328fbce
```
