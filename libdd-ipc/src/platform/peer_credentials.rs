// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Credentials of the connected peer, obtained once at connection time.
///
/// Shared by both platforms so that [`crate::PeerCredentials`] means one thing everywhere, even
/// though what fills it differs. On Unix the kernel supplies them - `SO_PEERCRED` on Linux, or
/// `LOCAL_PEERPID` plus a process lookup on macOS - and they are what authorization is decided
/// on.
///
/// Windows has no uid, and needs none: cross-user access is already refused by the operating
/// system before a connection exists. The pipe is created with a NULL security descriptor,
/// whose default named-pipe DACL grants `Everyone` read-only access, while connecting opens it
/// with `GENERIC_READ | GENERIC_WRITE` - so another user's `CreateFileA` fails with
/// `ERROR_ACCESS_DENIED`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PeerCredentials {
    pub pid: u32,
    /// A placeholder on Windows, not an identity: that platform has no uid. See the struct docs.
    pub uid: u32,
    /// A placeholder on Windows, as `uid` is.
    pub gid: u32,
}
