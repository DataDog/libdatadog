// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

const MAX_DOWNLOAD_BYTES: usize = 300 * 1024 * 1024;

pub(super) const DOWNLOADS: &[Sha256Download<'_>] = &[Sha256Download {
    url: "http://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/alpine-virt-3.24.0-x86_64.iso",
    max_len: MAX_DOWNLOAD_BYTES,
    sha256: [
        0x6c, 0xd1, 0xa3, 0x8a, 0xe0, 0x5c, 0xf9, 0x6a, 0x5d, 0x0c, 0xbb, 0x2d, 0xdd, 0x6c, 0x63,
        0x08, 0x34, 0xba, 0xbf, 0xec, 0xa1, 0xec, 0xc5, 0xd1, 0xf0, 0x5e, 0xc0, 0xb0, 0x6b, 0x88,
        0x61, 0x02,
    ],
}];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Sha256Download<'a> {
    pub(super) url: &'a str,
    pub(super) max_len: usize,
    pub(super) sha256: [u8; 32],
}
