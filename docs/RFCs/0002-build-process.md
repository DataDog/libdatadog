# RFC 0002: Building process proposal

## Context
As described in the [RFC](https://github.com/DataDog/libdatadog/blob/main/docs/RFCs/0001-delivering-different-features.md)
about delivering different features the current build process depends heavily on shell scripts which makes it too 
difficult in terms of managning the process itself like handling errors, dependendencies and features. Also shell
scripts by themselves poses some disadvantages like portability and maintanibilty.

This solution aims to get rid of those problems so the whole procedure depends as much as possible of built-in features 
of cargo making it less error prone and also improving its maintanibilty.

## Possible solutions
Since we need some sort of post build actions and cargo currently does not provide any built-in method to carry out these 
kind of actions we evaluated several options:

### Add a new crate to manage the build process
Add another crate in the workspace which will manage the postbuild actions and generate the artifacts. 

### Use cargo-make
cargo-make enables to define and configure sets of tasks and run them as a flow. A task is a command, script, rust
code, or other sub tasks to execute. Tasks can have dependencies which are also tasks that will be executed before the
task itself.

### Use cargo-post
A cargo wrapper which runs a post build script whenever the current build runs successfully.

## Proposed solution
Adding a new crate seems the most flexible solution to handle our current needs. This solution relies on a build.rs
script and a Cargo.toml files to handle the whole process. This way the cargo file will manage build dependencies,
versioning and features so dependencies are properly built with the selected features integrated. On the other hand
the build.rs will handle post compiling stuff like sanitizing binaries, invoking external tools to generate binaries
and assemble the final artifact.

### Implementation
As we want to make progress and have feedback as soon as possible we divided the work in two stages:

#### Stage 1 
Replicate all the work done in the current build scripts so we're able to generate the same artifacts and check the 
solution can be integrated seamlessly in the current repo state.

#### Stage 2
As this would cause some friction with other teams, we will move towards the single artifact release mentioned in this 
[RFC](https://github.com/DataDog/libdatadog/blob/main/docs/RFCs/delivering-different-features.md) in a later stage.

### Crate layout
Here is the new crate layout:
```
libdatadog/
├─ builder/
│  ├─ Cargo.toml
│  ├─ build.rs
├─ crashtracker/
├─ crashtracker-ffi/
├─ data_pipeline/
├─ .../
├─ windows/

```

### Use example

```
cargo build -p builder --features crashtracker-collector,crashtracker-receiver,cbindgen --release
```

### Artifact generated

```
vX.Y.Z/
├─ bin/
│  ├─ libdatadog-crashtracking-receiver
├─ include/
│  ├─ datadog/
│     ├─ common.h
│     ├─ profiling.h
│     ├─ telemetry.h
│     ├─ data-pipeline.h
├─ lib/
│  ├─ libdatadog.a
│  ├─ libdatadog.so
│  ├─ pkgconfig/
│     ├─ libdatadog.pc
```

```
libdatadogvX.Y.Z.tgz
```
