// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Authenticate IPC connections. Linux abstract sockets have no filesystem permissions, so the
//! listener checks peers according to its deployment:
//!
//! * [`AuthPolicy::OwnUid`]: a subprocess sidecar serves its own UID.
//! * [`AuthPolicy::HostDescendants`]: a thread listener accepts the host's UID or a descendant that
//!   dropped privileges. It then drops to the first worker's identity and requires later workers to
//!   match that UID and GID. See [`crate::setup::thread_listener`].
//! * [`AuthPolicy::OsEnforced`]: Windows named-pipe permissions restrict access before a connection
//!   reaches the listener.

use libdd_ipc::PeerCredentials;
use tracing::warn;

/// uid 0 is always accepted: a root peer can already read this process's memory and signal it,
/// so refusing its IPC connection protects nothing.
const ROOT_UID: u32 = 0;

/// Which peers a listener accepts. See the module docs for the reasoning behind each.
#[derive(Debug, Clone, Copy)]
pub enum AuthPolicy {
    /// Out-of-process sidecar: peers must hold this process's own uid.
    OwnUid,
    /// Thread listener: accept the host's UID or a descendant, then pin the served identity.
    HostDescendants { host_pid: u32 },
    /// The operating system already restricts who can connect.
    OsEnforced,
}

/// The outcome of authenticating one connection.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Serve the connection.
    Allow,
    /// Drop the connection.
    Deny,
}

pub struct ConnectionAuthorizer {
    policy: AuthPolicy,
    /// euid of the process hosting the listener.
    own_uid: u32,
    /// Identity chosen by the first worker. Later workers must match because the listener runs
    /// every request with these credentials.
    served: std::sync::OnceLock<(u32, u32)>,
}

impl ConnectionAuthorizer {
    /// Authenticator for an out-of-process sidecar, which serves its own uid.
    ///
    /// On Windows the pipe's DACL already restricts access, so no check is applied.
    pub fn for_spawned_sidecar() -> Self {
        #[cfg(unix)]
        let policy = AuthPolicy::OwnUid;
        #[cfg(not(unix))]
        let policy = AuthPolicy::OsEnforced;
        Self::new(policy)
    }

    /// Authenticator for a listener running inside the process it serves (thread mode).
    ///
    /// On Windows the pipe's DACL already restricts access, so no check is applied.
    pub fn for_in_process_listener() -> Self {
        #[cfg(unix)]
        // The listener runs in this process, so the tree peers must descend from is ours.
        // SAFETY: getpid() takes no arguments and cannot fail.
        let policy = AuthPolicy::HostDescendants {
            host_pid: unsafe { libc::getpid() } as u32,
        };
        #[cfg(not(unix))]
        let policy = AuthPolicy::OsEnforced;
        Self::new(policy)
    }

    pub fn new(policy: AuthPolicy) -> Self {
        Self {
            policy,
            own_uid: current_uid(),
            served: std::sync::OnceLock::new(),
        }
    }

    /// Set the served identity from the first accepted worker. Later calls leave it unchanged.
    pub fn set_served_identity(&self, uid: u32, gid: u32) {
        let _ = self.served.set((uid, gid));
    }

    /// Authenticate one accepted connection.
    pub fn authorize(&self, peer: &PeerCredentials) -> Decision {
        match self.policy {
            AuthPolicy::OsEnforced => Decision::Allow,
            AuthPolicy::OwnUid => self.decide_by_uid(peer),
            AuthPolicy::HostDescendants { host_pid } => self.authorize_descendant(peer, host_pid),
        }
    }

    /// Log the rejection of a connection. Kept next to the decision so the message always
    /// carries the uids needed to tell a misconfiguration from an intrusion.
    pub fn log_denied(&self, peer: &PeerCredentials, decision: Decision) {
        if decision == Decision::Deny {
            warn!(
                "IPC: rejected connection from pid {} (uid {}): this sidecar serves uid {}",
                peer.pid, peer.uid, self.own_uid
            );
        }
    }

    fn authorize_descendant(&self, peer: &PeerCredentials, host_pid: u32) -> Decision {
        if peer.uid == ROOT_UID {
            return Decision::Allow;
        }
        // Do not serve another pool with the first pool's credentials.
        if let Some((served_uid, served_gid)) = self.served.get() {
            return if peer.uid == *served_uid && peer.gid == *served_gid {
                Decision::Allow
            } else {
                Decision::Deny
            };
        }
        // Before the first worker, accept our own UID or a child that dropped privileges.
        if peer.uid == self.own_uid {
            return Decision::Allow;
        }
        if self.own_uid == ROOT_UID && peer_descends_from(peer.pid, host_pid) {
            return Decision::Allow;
        }
        Decision::Deny
    }

    /// Judge a peer against the only uid this sidecar serves: its own, plus root.
    fn decide_by_uid(&self, peer: &PeerCredentials) -> Decision {
        if peer.uid == self.own_uid || peer.uid == ROOT_UID {
            Decision::Allow
        } else {
            Decision::Deny
        }
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: geteuid() takes no arguments and cannot fail.
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    // Windows has no uid; AuthPolicy::OsEnforced never consults it.
    0
}

#[cfg(unix)]
fn peer_descends_from(pid: u32, ancestor: u32) -> bool {
    libdd_ipc::platform::process::is_descendant_of(pid, ancestor)
}

#[cfg(not(unix))]
fn peer_descends_from(_pid: u32, _ancestor: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(pid: u32, uid: u32) -> PeerCredentials {
        PeerCredentials { pid, uid, gid: uid }
    }

    fn other_uid() -> u32 {
        // Any uid that is neither ours nor root, so it must be rejected.
        if current_uid() == 12345 {
            12346
        } else {
            12345
        }
    }

    #[test]
    fn os_enforced_allows_everyone() {
        let auth = ConnectionAuthorizer::new(AuthPolicy::OsEnforced);
        assert_eq!(auth.authorize(&peer(1, other_uid())), Decision::Allow);
    }

    /// A subprocess sidecar is a fork+exec of its spawner, so the uid it serves is its own for
    /// life. Nothing about a peer other than its uid can change that.
    #[test]
    fn own_uid_serves_only_that_uid() {
        let auth = ConnectionAuthorizer::new(AuthPolicy::OwnUid);

        assert_eq!(auth.authorize(&peer(99, current_uid())), Decision::Allow);
        assert_eq!(auth.authorize(&peer(98, ROOT_UID)), Decision::Allow);
        assert_eq!(auth.authorize(&peer(97, other_uid())), Decision::Deny);
    }

    #[cfg(unix)]
    #[test]
    fn in_process_listener_accepts_descendants_only_when_root() {
        let own_pid = std::process::id();
        let auth = ConnectionAuthorizer::new(AuthPolicy::HostDescendants { host_pid: own_pid });

        // Our own uid, and root, are accepted without consulting the process table.
        assert_eq!(auth.authorize(&peer(1, current_uid())), Decision::Allow);
        assert_eq!(auth.authorize(&peer(1, ROOT_UID)), Decision::Allow);

        // A foreign uid that does not descend from the host is refused either way.
        assert_eq!(auth.authorize(&peer(1, other_uid())), Decision::Deny);

        // The parent-chain walk exists for one shape only: a root listener serving workers that
        // dropped privileges. A non-root listener has no business serving a foreign uid at all,
        // so the same peer is a Deny here and an Allow when the suite runs as root. This process
        // is trivially its own descendant, standing in for a forked worker.
        let worker = peer(own_pid, other_uid());
        let expected = if current_uid() == ROOT_UID {
            Decision::Allow
        } else {
            Decision::Deny
        };
        assert_eq!(auth.authorize(&worker), expected);
    }

    #[cfg(unix)]
    #[test]
    fn served_identity_pins_the_accepted_uid_and_gid() {
        let own_pid = std::process::id();
        let auth = ConnectionAuthorizer::new(AuthPolicy::HostDescendants { host_pid: own_pid });
        auth.set_served_identity(33, 33);

        assert_eq!(auth.authorize(&peer(own_pid, 33)), Decision::Allow);
        // A different UID is refused even for a descendant of the host.
        assert_eq!(auth.authorize(&peer(own_pid, 34)), Decision::Deny);
        // The GID must match too.
        assert_eq!(
            auth.authorize(&PeerCredentials {
                pid: own_pid,
                uid: 33,
                gid: 34
            }),
            Decision::Deny
        );
        assert_eq!(auth.authorize(&peer(own_pid, ROOT_UID)), Decision::Allow);
    }
}
