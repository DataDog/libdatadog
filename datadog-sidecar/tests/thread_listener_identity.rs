// Copyright 2026 Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use datadog_sidecar::auth::{ConnectionAuthorizer, Decision};
use libdd_ipc::PeerCredentials;

#[test]
fn thread_listener_restarts_keep_the_served_identity() {
    let peer = PeerCredentials {
        pid: std::process::id(),
        // Use a non-root identity: root is intentionally accepted regardless of the pin.
        uid: unsafe { libc::geteuid() }.max(1),
        gid: unsafe { libc::getegid() },
    };
    let first = ConnectionAuthorizer::for_in_process_listener();
    first.set_served_identity(peer.uid, peer.gid);
    assert_eq!(first.authorize(&peer), Decision::Allow);
    drop(first);

    let restarted = ConnectionAuthorizer::for_in_process_listener();
    assert_eq!(restarted.authorize(&peer), Decision::Allow);
    assert_eq!(
        restarted.authorize(&PeerCredentials {
            gid: peer.gid.wrapping_add(1),
            ..peer
        }),
        Decision::Deny,
        "a restarted listener must still enforce the first worker's GID"
    );
    assert_eq!(
        restarted.authorize(&PeerCredentials {
            uid: peer.uid.wrapping_add(1),
            ..peer
        }),
        Decision::Deny
    );
}
