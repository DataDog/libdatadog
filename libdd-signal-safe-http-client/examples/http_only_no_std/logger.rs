// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use rustix::{io, stdio};

const HEX: &[u8; 16] = b"0123456789abcdef";

pub(super) struct Logger;

impl Logger {
    pub(super) fn line(message: &str) {
        write_str(message);
        newline();
    }

    pub(super) fn field_str(name: &str, value: &str) {
        write_str(name);
        write_str(": ");
        write_str(value);
        newline();
    }

    pub(super) fn field_usize(name: &str, value: usize) {
        write_str(name);
        write_str(": ");
        write_usize(value);
        newline();
    }

    pub(super) fn download_start(index: usize, total: usize, url: &str) {
        write_str("download ");
        write_usize(index);
        write_str("/");
        write_usize(total);
        write_str(": ");
        write_str(url);
        newline();
    }

    pub(super) fn http_status(status: u16) {
        write_str("http status: ");
        write_usize(usize::from(status));
        newline();
    }

    pub(super) fn progress(read: usize, expected: usize) {
        write_str("streamed: ");
        write_usize(read);
        write_str("/");
        write_usize(expected);
        write_str(" bytes");
        newline();
    }

    pub(super) fn sha256(digest: &[u8]) {
        write_str("sha256: ");
        for byte in digest {
            let hi = usize::from(byte >> 4);
            let lo = usize::from(byte & 0x0f);
            write_bytes(&[HEX[hi], HEX[lo]]);
        }
        newline();
    }
}

fn write_usize(mut value: usize) {
    if value == 0 {
        write_str("0");
        return;
    }

    let mut buffer = [0_u8; 39];
    let mut next = buffer.len();
    while value != 0 {
        next -= 1;
        buffer[next] = b'0' + u8::try_from(value % 10).unwrap_or_default();
        value /= 10;
    }

    write_bytes(&buffer[next..]);
}

fn write_str(value: &str) {
    write_bytes(value.as_bytes());
}

fn newline() {
    write_bytes(b"\n");
}

fn write_bytes(mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: This example is single-threaded and assumes the inherited
        // stdout file descriptor has not been closed or reused.
        let stdout = unsafe { stdio::stdout() };
        let Ok(written) = io::write(stdout, bytes) else {
            return;
        };
        if written == 0 {
            return;
        }
        bytes = &bytes[written..];
    }
}
