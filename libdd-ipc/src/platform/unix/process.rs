// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Process-table lookups used when authenticating IPC peers.
//!
//! Note that we do not consider any pid-reuse races here, the reads are assumed to be done
//! within milliseconds.

/// The peer's effective uid, or `None` if the pid is gone or unreadable.
pub fn effective_uid_of(pid: u32) -> Option<u32> {
    proc_info(pid).map(|info| info.uid)
}

/// The peer's effective uid and gid, or `None` if the pid is gone or unreadable.
pub fn effective_ids_of(pid: u32) -> Option<(u32, u32)> {
    proc_info(pid).map(|info| (info.uid, info.gid))
}

/// The peer's parent pid, or `None` if the pid is gone or unreadable.
pub fn parent_pid_of(pid: u32) -> Option<u32> {
    proc_info(pid).map(|info| info.ppid)
}

/// `true` if `pid` is `ancestor` itself or appears below it in the parent chain.
///
/// The walk is bounded: a corrupted or cyclic chain terminates instead of spinning. Reaching
/// pid 1 (or 0) without finding `ancestor` means "not a descendant".
pub fn is_descendant_of(pid: u32, ancestor: u32) -> bool {
    /// Generous compared to any real supervisor tree, cheap to bound.
    const MAX_DEPTH: usize = 64;

    let mut current = pid;
    for _ in 0..MAX_DEPTH {
        if current == ancestor {
            return true;
        }
        if current <= 1 {
            return false;
        }
        match parent_pid_of(current) {
            Some(parent) => current = parent,
            None => return false,
        }
    }
    false
}

struct ProcInfo {
    uid: u32,
    gid: u32,
    ppid: u32,
}

#[cfg(target_os = "linux")]
fn proc_info(pid: u32) -> Option<ProcInfo> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let mut uid = None;
    let mut gid = None;
    let mut ppid = None;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            // "Uid:\t<real>\t<effective>\t<saved>\t<fs>"
            uid = rest.split_whitespace().nth(1).and_then(|v| v.parse().ok());
        } else if let Some(rest) = line.strip_prefix("Gid:") {
            gid = rest.split_whitespace().nth(1).and_then(|v| v.parse().ok());
        } else if let Some(rest) = line.strip_prefix("PPid:") {
            ppid = rest.trim().parse().ok();
        }
        if uid.is_some() && gid.is_some() && ppid.is_some() {
            break;
        }
    }
    Some(ProcInfo {
        uid: uid?,
        gid: gid?,
        ppid: ppid?,
    })
}

#[cfg(not(target_os = "linux"))]
fn proc_info(pid: u32) -> Option<ProcInfo> {
    // structs/constants are as defined in `<sys/proc_info.h>`.
    #[repr(C)]
    #[derive(Default)]
    struct ProcBsdShortInfo {
        pbsi_pid: u32,
        pbsi_ppid: u32,
        pbsi_pgid: u32,
        pbsi_status: u32,
        pbsi_comm: [libc::c_char; 16], // MAXCOMLEN
        pbsi_flags: u32,
        pbsi_uid: libc::uid_t,
        pbsi_gid: libc::gid_t,
        pbsi_ruid: libc::uid_t,
        pbsi_rgid: libc::gid_t,
        pbsi_svuid: libc::uid_t,
        pbsi_svgid: libc::gid_t,
        pbsi_rfu: u32,
    }

    const PROC_PIDT_SHORTBSDINFO: libc::c_int = 13;

    extern "C" {
        fn proc_pidinfo(
            pid: libc::c_int,
            flavor: libc::c_int,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }

    let mut info = ProcBsdShortInfo::default();
    let size = size_of::<ProcBsdShortInfo>() as libc::c_int;
    // SAFETY: `info` is a live, correctly sized buffer for the requested flavor, and
    // `proc_pidinfo` writes at most `size` bytes into it.
    let written = unsafe {
        proc_pidinfo(
            pid as libc::c_int,
            PROC_PIDT_SHORTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    // A dead or unreadable pid yields 0 (or -1); a short write means a struct mismatch, which
    // must not be read as a uid of 0.
    if written != size {
        return None;
    }
    Some(ProcInfo {
        uid: info.pbsi_uid,
        gid: info.pbsi_gid,
        ppid: info.pbsi_ppid,
    })
}

#[cfg(test)]
mod tests {
    // These resolve real pids: through `/proc` on Linux, `proc_pidinfo` on macOS, and
    // `getpid`/`getppid` on both. Miri implements none of them - it refuses the foreign calls
    // outright - so there is nothing here it can evaluate.
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn own_process_resolves_to_own_credentials() {
        let pid = std::process::id();
        assert_eq!(
            effective_uid_of(pid),
            Some(unsafe { libc::geteuid() }),
            "effective_uid_of(self) must match geteuid()"
        );
        assert_eq!(
            parent_pid_of(pid),
            Some(unsafe { libc::getppid() } as u32),
            "parent_pid_of(self) must match getppid()"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn foreign_process_is_readable() {
        // pid 1 always exists and is usually owned by another user; the macOS peer-uid path
        // depends on being able to read it without privileges. Its uid is not asserted
        // because a container may well run pid 1 as the test user.
        assert!(effective_uid_of(1).is_some(), "pid 1 must resolve to a uid");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn dead_pid_resolves_to_none() {
        // Well above any /proc/sys/kernel/pid_max default, so it cannot be live.
        assert_eq!(effective_uid_of(0x7FFF_FFFF), None);
        assert_eq!(parent_pid_of(0x7FFF_FFFF), None);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn descendant_chain_is_walked() {
        let pid = std::process::id();
        assert!(is_descendant_of(pid, pid), "a pid is its own ancestor");

        let parent = unsafe { libc::getppid() } as u32;
        // A process that is its own init - pid 1, as in a container started without one - has
        // no chain to walk: `getppid()` reports 0, and "pid 1 is not a descendant of self" is
        // not even true of it. Only the self-ancestor case above is meaningful there.
        if pid == 1 || parent == 0 {
            eprintln!(
                "skipping the rest of descendant_chain_is_walked: \
                 running as pid 1, which has no parent to walk to"
            );
            return;
        }

        assert!(
            is_descendant_of(pid, parent),
            "self must be a descendant of its parent"
        );
        assert!(
            is_descendant_of(pid, 1),
            "every process descends from pid 1"
        );
        assert!(
            !is_descendant_of(1, pid),
            "pid 1 must not be a descendant of self"
        );
    }
}
