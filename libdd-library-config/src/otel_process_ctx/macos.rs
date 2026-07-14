// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use portable_atomic::AtomicU128;

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
compile_error!("OTel process context only supports aarch64 and x86_64 on macOS");
#[cfg(not(target_endian = "little"))]
compile_error!("OTel process context requires a little-endian macOS target");

pub(super) type AtomicPublishedHeader = AtomicU128;

const _: () = {
    // Both supported macOS architectures have native 128-bit atomics. Keep this assertion so a
    // target change cannot silently select portable-atomic's software-lock fallback.
    assert!(AtomicPublishedHeader::is_always_lock_free());
    assert!(size_of::<AtomicPublishedHeader>() == size_of::<u128>());
    assert!(align_of::<AtomicPublishedHeader>() == 16);
    assert!(size_of::<usize>() == size_of::<u64>());
};

// The low 64 bits contain the header address and the next 32 bits contain the publisher PID.
// Keeping them in one value lets readers observe both through a single atomic load.
#[cfg(feature = "process-context-reader")]
pub(super) const HEADER_ADDRESS_MASK: u128 = u64::MAX as u128;
pub(super) const PUBLISHER_PID_SHIFT: u32 = u64::BITS;
