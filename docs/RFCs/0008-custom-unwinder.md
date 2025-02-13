# RFC 0008: Crashtracker unwinding (Version 0.1). 

## Context

In the scope of [incident 34148](https://dd.enterprise.slack.com/archives/C088R4S25M5), we have incomplete unwinding on musl. Our top priority is to enable a build of dd-trace-php that allows unwinds crashes on musl.
I recommend [this issue](https://github.com/rust-lang/backtrace-rs/issues/698) for more context on the underlying unwinding issue.

This is only an issue for the languages that do not have a built-in unwinding (like Java).

## Solution proposed

Unwinding starting from the context of the signal handler allows us to get the stacktrace beyond the signal handler. The issue above details some of the experiments I have performed.

### Unwinding libraries

Using [libunwind](https://github.com/libunwind/libunwind/) is not mandatory. A full rust solution can be considered using framehop (which is built on top of Gimli). We have experience using libunwind and debugging it. libunwind is the unwinder for the .NET profiler.
When swapping for a different library we should consider maintenance, internal knowledge and the redundancy of what we are shipping.

### Packaging of libunwind

As this is a C library used from Rust, we need to declare the functions we use in libdatadog for the different architectures. This requires some adjustements as the functions have architecture specific names.

We can rely on bindgen to generate the bindings. However as this adds complexity to the builds I favoured declaring the minimal set of functions required for the unwinding.
The libunwind-sys crate did not work correctly when I tried adding it to libdatadog though it is a good source to generate relevant bindings.

We should statically link libunwind and make symbols invisible to our users.
The link of libunwind requires `libgcc_s.so.1`. This does not change anythinng as we already needed this dependency (as we are using backtrace mechanisms).

Size impacts looking at libdatadog_profiling.so
- +1.3 Meg on the shared library (9 Megs total)

TODO: measures are ongoing with the PHP binary.

### Deployment 

We propose to deploy the feature OFF by default. We can then check with the customer to enable this and get the musl crash locations.
If this is a success, we can roll out progressively the change.

### Out of scope

Signal safety is not discussed.
The current implementation is not signal safe. The long term direction is to move the unwinding out of the process.

Shipping libunwund so that .NET folks can reuse it.
This should come in a second phase.

Fixing backtrace-rs
The ideal solution would be to solve the upstream issue. Unwinding from signals in musl is crucial.
I currently do not see an obvious way to do this with the [gcc_s functions](https://refspecs.linuxfoundation.org/LSB_4.1.0/LSB-Core-generic/LSB-Core-generic/libgcc-sman.html).
