#!/bin/sh
# Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

set -xe
cd /build; 

## group Cargo files and required stubs
mkdir -p /output/cargo/

cp Cargo.lock /output/cargo/
find . -name "Cargo.toml" | xargs -n 1 sh -c 'export OUT=/output/cargo/$1; mkdir -p $(dirname $OUT); cp $1 $OUT; echo $OUT' copy_cargo
find . -name "Cargo.toml" | sed -e s#Cargo.toml#src/lib.rs#g | xargs -n 1 sh -c 'export OUT=/output/cargo/$1; mkdir -p $(dirname $OUT); touch $OUT; echo $OUT' create_lib_stubs
find . -wholename "*/benches/*.rs" -o -wholename "*/src/bin/*.rs" -o -wholename "*/examples/*.rs" | xargs -n 1 sh -c 'export OUT=/output/cargo/$1; mkdir -p $(dirname $OUT); touch $OUT; echo $OUT' create_required_stubs

## group rust source files
mkdir -p /output/rs_src
find . -name "*.rs" | xargs -n 1 sh -c 'export OUT=/output/rs_src/$1; mkdir -p $(dirname $OUT); cp $1 $OUT; echo $OUT' copy_sources

## groups other source files
mkdir -p /output/other_src
find . -name "*.c" -o -name "*.h" -o -name "*.sh" | xargs -n 1 sh -c 'export OUT=/output/other_src/$1; mkdir -p $(dirname $OUT); cp $1 $OUT; echo $OUT' copy_sources

find /output
