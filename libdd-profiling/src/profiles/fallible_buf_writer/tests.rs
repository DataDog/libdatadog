// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use bolero::generator::TypeGenerator;
use std::io::BufWriter as StdBufWriter;

#[derive(Clone, Debug, Default, PartialEq)]
struct RecordingWriter {
    writes: Vec<Vec<u8>>,
    flushes: usize,
    max_write_len: Option<usize>,
    fail_write_on: Option<usize>,
    fail_flush_on: Option<usize>,
}

impl RecordingWriter {
    fn contents(&self) -> Vec<u8> {
        self.writes.concat()
    }
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let write_call = self.writes.len() + 1;
        if self.fail_write_on == Some(write_call) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "scripted write failure",
            ));
        }

        let len = self.max_write_len.unwrap_or(buf.len()).min(buf.len());
        self.writes.push(buf[..len].to_vec());
        Ok(len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        if self.fail_flush_on == Some(self.flushes) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "scripted flush failure",
            ));
        }
        Ok(())
    }
}

#[test]
fn try_with_capacity_reserves_the_requested_capacity() {
    let writer = BufWriter::try_with_capacity(64, Vec::<u8>::new()).unwrap();
    assert!(writer.capacity() >= 64);
}

#[test]
fn buffers_small_writes_until_flush() {
    let mut writer = BufWriter::try_with_capacity(16, RecordingWriter::default()).unwrap();

    writer.write_all(b"hello").unwrap();
    assert!(writer.get_ref().writes.is_empty());
    assert_eq!(writer.buffer(), b"hello");

    writer.flush().unwrap();
    assert_eq!(writer.get_ref().contents(), b"hello");
    assert_eq!(writer.get_ref().flushes, 1);
    assert!(writer.buffer().is_empty());
}

#[test]
fn large_writes_bypass_the_buffer() {
    let mut writer = BufWriter::try_with_capacity(4, RecordingWriter::default()).unwrap();

    writer.write_all(b"hello").unwrap();

    assert_eq!(writer.get_ref().contents(), b"hello");
    assert!(writer.buffer().is_empty());
}

#[test]
fn into_inner_flushes_buffered_data() {
    let mut writer = BufWriter::try_with_capacity(16, Vec::new()).unwrap();

    writer.write_all(b"hello").unwrap();
    let inner = writer.into_inner().unwrap();

    assert_eq!(inner, b"hello");
}

#[test]
fn zero_capacity_writes_directly() {
    let mut writer = BufWriter::try_with_capacity(0, RecordingWriter::default()).unwrap();

    writer.write_all(b"a").unwrap();

    assert_eq!(writer.get_ref().contents(), b"a");
    assert!(writer.buffer().is_empty());
}

#[derive(Clone, Debug, TypeGenerator)]
enum FuzzOperation {
    Write(Vec<u8>),
    WriteAll(Vec<u8>),
    WriteVectored(Vec<Vec<u8>>),
    Flush,
}

#[derive(Clone, Debug, TypeGenerator)]
struct FuzzCase {
    capacity: u8,
    max_write_len: u8,
    fail_write_on: Option<u8>,
    fail_flush_on: Option<u8>,
    operations: Vec<FuzzOperation>,
}

#[derive(Debug, PartialEq)]
enum OperationResult {
    Usize(Result<usize, io::ErrorKind>),
    Unit(Result<(), io::ErrorKind>),
}

#[derive(Debug, PartialEq)]
struct WriterState {
    contents: Vec<u8>,
    writes: Vec<Vec<u8>>,
    flushes: usize,
}

#[derive(Debug, PartialEq)]
struct Observation {
    result: OperationResult,
    buffer: Vec<u8>,
    capacity: usize,
    inner: WriterState,
}

#[derive(Debug, PartialEq)]
enum FinalObservation {
    Ok(WriterState),
    Err {
        error: io::ErrorKind,
        buffer: Vec<u8>,
        capacity: usize,
        inner: WriterState,
    },
}

#[derive(Debug, PartialEq)]
struct RunObservations {
    observations: Vec<Observation>,
    final_observation: FinalObservation,
}

fn writer_from_fuzz_case(case: &FuzzCase) -> RecordingWriter {
    RecordingWriter {
        writes: Vec::new(),
        flushes: 0,
        max_write_len: Some(usize::from(case.max_write_len % 17)),
        fail_write_on: case.fail_write_on.map(|n| usize::from(n % 128) + 1),
        fail_flush_on: case.fail_flush_on.map(|n| usize::from(n % 128) + 1),
    }
}

fn writer_state(writer: &RecordingWriter) -> WriterState {
    WriterState {
        contents: writer.contents(),
        writes: writer.writes.clone(),
        flushes: writer.flushes,
    }
}

fn result_usize(result: io::Result<usize>) -> OperationResult {
    OperationResult::Usize(result.map_err(|error| error.kind()))
}

fn result_unit(result: io::Result<()>) -> OperationResult {
    OperationResult::Unit(result.map_err(|error| error.kind()))
}

fn observe_std(writer: &StdBufWriter<RecordingWriter>, result: OperationResult) -> Observation {
    Observation {
        result,
        buffer: writer.buffer().to_vec(),
        capacity: writer.capacity(),
        inner: writer_state(writer.get_ref()),
    }
}

fn observe_fallible(writer: &BufWriter<RecordingWriter>, result: OperationResult) -> Observation {
    Observation {
        result,
        buffer: writer.buffer().to_vec(),
        capacity: writer.capacity(),
        inner: writer_state(writer.get_ref()),
    }
}

fn run_std(case: &FuzzCase) -> RunObservations {
    let capacity = usize::from(case.capacity % 33);
    let mut writer = StdBufWriter::with_capacity(capacity, writer_from_fuzz_case(case));
    let mut observations = Vec::new();

    for operation in case.operations.iter().take(if cfg!(miri) { 4 } else { 64 }) {
        let observation = match operation {
            FuzzOperation::Write(buf) => {
                let result = writer.write(buf);
                observe_std(&writer, result_usize(result))
            }
            FuzzOperation::WriteAll(buf) => {
                let result = writer.write_all(buf);
                observe_std(&writer, result_unit(result))
            }
            FuzzOperation::WriteVectored(bufs) => {
                let bufs = bufs.iter().map(|buf| IoSlice::new(buf)).collect::<Vec<_>>();
                let result = writer.write_vectored(&bufs);
                observe_std(&writer, result_usize(result))
            }
            FuzzOperation::Flush => {
                let result = writer.flush();
                observe_std(&writer, result_unit(result))
            }
        };
        observations.push(observation);
    }

    let final_observation = match writer.into_inner() {
        Ok(writer) => FinalObservation::Ok(writer_state(&writer)),
        Err(error) => {
            let error_kind = error.error().kind();
            let writer = error.into_inner();
            FinalObservation::Err {
                error: error_kind,
                buffer: writer.buffer().to_vec(),
                capacity: writer.capacity(),
                inner: writer_state(writer.get_ref()),
            }
        }
    };

    RunObservations {
        observations,
        final_observation,
    }
}

fn run_fallible(case: &FuzzCase) -> RunObservations {
    let capacity = usize::from(case.capacity % 33);
    let mut writer = BufWriter::try_with_capacity(capacity, writer_from_fuzz_case(case)).unwrap();
    let mut observations = Vec::new();

    for operation in case.operations.iter().take(if cfg!(miri) { 4 } else { 64 }) {
        let observation = match operation {
            FuzzOperation::Write(buf) => {
                let result = writer.write(buf);
                observe_fallible(&writer, result_usize(result))
            }
            FuzzOperation::WriteAll(buf) => {
                let result = writer.write_all(buf);
                observe_fallible(&writer, result_unit(result))
            }
            FuzzOperation::WriteVectored(bufs) => {
                let bufs = bufs.iter().map(|buf| IoSlice::new(buf)).collect::<Vec<_>>();
                let result = writer.write_vectored(&bufs);
                observe_fallible(&writer, result_usize(result))
            }
            FuzzOperation::Flush => {
                let result = writer.flush();
                observe_fallible(&writer, result_unit(result))
            }
        };
        observations.push(observation);
    }

    let final_observation = match writer.into_inner() {
        Ok(writer) => FinalObservation::Ok(writer_state(&writer)),
        Err(error) => {
            let error_kind = error.error().kind();
            let writer = error.into_inner();
            FinalObservation::Err {
                error: error_kind,
                buffer: writer.buffer().to_vec(),
                capacity: writer.capacity(),
                inner: writer_state(writer.get_ref()),
            }
        }
    };

    RunObservations {
        observations,
        final_observation,
    }
}

#[test]
fn fuzz_matches_std_buf_writer_after_creation() {
    bolero::check!().with_type::<FuzzCase>().for_each(|case| {
        let fallible = run_fallible(case);
        let std = run_std(case);

        assert_eq!(fallible.observations.len(), std.observations.len());
        for (idx, (fallible, std)) in fallible
            .observations
            .iter()
            .zip(std.observations.iter())
            .enumerate()
        {
            assert_eq!(
                fallible,
                std,
                "operation {idx} diverged: {:?}",
                case.operations.get(idx)
            );
        }
        assert_eq!(fallible.final_observation, std.final_observation);
    });
}
