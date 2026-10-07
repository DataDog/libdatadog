// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::service::ffe_submission::FfeSubmissionStatus as Status;
use crate::service::{
    FfeFlagEvaluationBatch, FfeFlagEvaluationEvent, FfeTelemetryContext, FlagKey,
};
use libdd_ipc::{SeqpacketConn, codec};

fn request() -> SidecarInterfaceRequest {
    SidecarInterfaceRequest::EnqueueActions {
        instance_id: InstanceId::new("ffe", "runtime"),
        queue_id: QueueId::from(1),
        actions: vec![SidecarAction::FfeFlagEvaluationBatch(
            FfeFlagEvaluationBatch {
                context: FfeTelemetryContext {
                    service: "service".into(),
                    env: "test".into(),
                    version: "1".into(),
                },
                flag_evaluations: vec![FfeFlagEvaluationEvent {
                    timestamp: 100,
                    flag: FlagKey { key: "flag".into() },
                    first_evaluation: 100,
                    last_evaluation: 100,
                    evaluation_count: 1,
                    variant: None,
                    allocation: None,
                    targeting_rule: None,
                    targeting_key: Some("subject".into()),
                    context: None,
                    error: None,
                    runtime_default_used: false,
                    observe_full_evaluation_data: false,
                    is_degraded: false,
                    field_omissions: Default::default(),
                }],
            },
        )],
    }
}

fn pair() -> (SidecarSender, SeqpacketConn) {
    let (conn, peer) = SeqpacketConn::socketpair().unwrap();
    (SidecarSender::new(SidecarInterfaceChannel::new(conn)), peer)
}

fn receive(peer: &SeqpacketConn) -> SidecarInterfaceRequest {
    let mut buf = vec![0; libdd_ipc::max_message_size()];
    let (len, fds) = peer.try_recv_raw(&mut buf).unwrap();
    assert!(fds.is_empty());
    codec::decode(&buf[..len]).unwrap()
}

fn event_mut(request: &mut SidecarInterfaceRequest) -> &mut FfeFlagEvaluationEvent {
    let SidecarInterfaceRequest::EnqueueActions { actions, .. } = request else {
        panic!("wrong request")
    };
    let SidecarAction::FfeFlagEvaluationBatch(batch) = &mut actions[0] else {
        panic!("wrong action")
    };
    &mut batch.flag_evaluations[0]
}

#[test]
#[cfg_attr(miri, ignore)]
fn ffe_advertised_size_fallback_preserves_count_and_omits_context() {
    let (mut sender, peer) = pair();
    let mut oversized = request();
    let event = event_mut(&mut oversized);
    event.targeting_key = Some("private-canary".repeat(libdd_ipc::max_message_size() / 14 + 1));
    event.context = Some(crate::service::FlagEvalEventContext {
        evaluation: Some(r#"{"email":"private-canary"}"#.into()),
        dd: None,
    });
    event.observe_full_evaluation_data = true;
    assert_eq!(sender.try_submit_ffe(|| Ok(oversized)), Status::Accepted);
    let mut received = receive(&peer);
    let event = event_mut(&mut received);
    assert!(event.is_degraded);
    assert!(event.context.is_none());
    assert!(event.targeting_key.is_none());
    assert_eq!(event.evaluation_count, 1);
    assert_eq!(event.flag.key, "flag");
    assert!(event.observe_full_evaluation_data);
    assert_eq!(sender.channel.0.outstanding(), 1);
    let encoded = bincode::serialize(&received).unwrap();
    assert!(
        !encoded
            .windows(b"private-canary".len())
            .any(|w| w == b"private-canary")
    );
}

#[test]
#[cfg(target_os = "linux")]
#[cfg_attr(miri, ignore)]
fn ffe_kernel_size_fallback_runs_admission_and_ack_accounting_once() {
    let (mut sender, peer) = pair();
    sender.max_outstanding = 21;
    outstanding(&mut sender, &peer, 11);
    sender.enqueue_actions_counter = 9;
    let bytes: libc::c_int = 4096;
    // SAFETY: a live socket and a valid pointer/length describing `bytes`.
    assert_eq!(
        unsafe {
            libc::setsockopt(
                sender.channel.0.conn.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&bytes as *const libc::c_int).cast(),
                std::mem::size_of_val(&bytes).try_into().unwrap(),
            )
        },
        0
    );
    let mut oversized = request();
    event_mut(&mut oversized).targeting_key = Some("x".repeat(16 * 1024));
    assert!(bincode::serialized_size(&oversized).unwrap() < libdd_ipc::max_message_size() as u64);
    assert_eq!(sender.try_submit_ffe(|| Ok(oversized)), Status::Accepted);
    let mut received = receive(&peer);
    let event = event_mut(&mut received);
    assert!(event.is_degraded);
    assert!(event.targeting_key.is_none());
    assert_eq!(event.evaluation_count, 1);
    assert_eq!(sender.enqueue_actions_counter, 0);
    assert_eq!(sender.channel.0.outstanding(), 12);
    assert!(!sender.channel.0.is_closed());
    assert_eq!(
        peer.try_recv_raw(&mut [0; 1024]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

// Send real ordinary requests and drain the peer without ACKing, keeping the
// shared outstanding count high without conflating it with socket capacity.
fn outstanding(sender: &mut SidecarSender, peer: &SeqpacketConn, count: u64) {
    for _ in 0..count {
        assert!(sender.channel.0.try_send(codec::encode(&request()), &[]));
        receive(peer);
    }
}

fn is_fill_refusal(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::WouldBlock {
        return true;
    }
    // Only the test setup treats ENOBUFS as a reason to keep filling. Production
    // retains its existing policy of marking this error as a closed connection.
    #[cfg(target_os = "macos")]
    if error.raw_os_error() == Some(libc::ENOBUFS) {
        return true;
    }
    false
}

fn full_socket(sender: &SidecarSender, peer: &SeqpacketConn) {
    // Small, socket-local buffers keep filling cheap without changing the
    // process-wide message limit (as set_sndbuf_size would).
    let capacity: libc::c_int = 16 * 1024;
    for fd in [sender.channel.0.conn.as_raw_fd(), peer.as_raw_fd()] {
        for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
            // SAFETY: the descriptor is live and the pointer/length describe capacity.
            assert_eq!(
                unsafe {
                    libc::setsockopt(
                        fd,
                        libc::SOL_SOCKET,
                        option,
                        (&capacity as *const libc::c_int).cast(),
                        std::mem::size_of_val(&capacity).try_into().unwrap(),
                    )
                },
                0
            );
        }
    }

    // WouldBlock or macOS ENOBUFS may be transient. Require repeated one-byte
    // refusals, resetting after progress, rather than trusting one.
    const REQUIRED_REFUSALS: usize = 64;
    let mut packet_size = 1024;
    let mut refusals = 0;
    let mut queued = 0;
    for _ in 0..10_000 {
        match sender
            .channel
            .0
            .conn
            .try_send_raw(vec![0; packet_size], &[])
        {
            Ok(()) => {
                queued += 1;
                refusals = 0;
            }
            // A rejected 1 KiB datagram can leave room for our smaller requests
            // on macOS. Fill that remaining space before asserting backpressure.
            Err(e) if is_fill_refusal(&e) && packet_size > 1 => {
                packet_size = 1;
            }
            Err(e) if is_fill_refusal(&e) => {
                refusals += 1;
                if refusals == REQUIRED_REFUSALS {
                    assert!(queued > 0, "no data queued before repeated refusals");
                    return;
                }
            }
            Err(e) => panic!("unexpected fill failure: {e}"),
        }
    }
    panic!("socket never became full");
}

#[test]
#[cfg(target_os = "linux")]
#[cfg_attr(miri, ignore)]
fn ffe_kernel_size_rejection_preserves_connection_for_next_observation() {
    let (mut sender, peer) = pair();
    // Change only this socket, not the process-wide advertised message limit.
    let bytes: libc::c_int = 4096;
    // SAFETY: the descriptor is live and the pointer/length describe `bytes`.
    assert_eq!(
        unsafe {
            libc::setsockopt(
                sender.channel.0.conn.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&bytes as *const libc::c_int).cast(),
                std::mem::size_of_val(&bytes).try_into().unwrap(),
            )
        },
        0
    );
    let mut oversized = request();
    if let SidecarInterfaceRequest::EnqueueActions { instance_id, .. } = &mut oversized {
        instance_id.runtime_id = "x".repeat(16 * 1024);
    }
    assert!(bincode::serialized_size(&oversized).unwrap() < libdd_ipc::max_message_size() as u64);
    assert_eq!(
        sender.try_submit_ffe(|| Ok(oversized)),
        Status::PayloadTooLarge
    );
    assert_eq!(sender.channel.0.outstanding(), 0);
    assert_eq!(sender.check_ffe_submission(), Status::Ready);
    assert_eq!(sender.try_submit_ffe(|| Ok(request())), Status::Accepted);
    receive(&peer);
    assert_eq!(sender.channel.0.outstanding(), 1);
}

#[test]
#[cfg_attr(miri, ignore)]
fn ffe_capacity_rejects_before_build_and_advisory_check_recovers_with_ack() {
    let (mut sender, peer) = pair();
    sender.max_outstanding = 21;
    outstanding(&mut sender, &peer, 21);
    assert_eq!(sender.check_ffe_submission(), Status::QueueFull);
    assert_eq!(
        sender.try_submit_ffe(|| panic!("must not copy")),
        Status::QueueFull
    );
    peer.try_send_raw(vec![0], &[]).unwrap();
    assert_eq!(sender.check_ffe_submission(), Status::Ready);
    assert_eq!(sender.channel.0.outstanding(), 20);
}

#[test]
#[cfg_attr(miri, ignore)]
fn ffe_shedding_advances_only_once_per_submission_and_never_bypasses_cap() {
    let (mut sender, peer) = pair();
    sender.max_outstanding = 21;
    outstanding(&mut sender, &peer, 20);
    for _ in 0..9 {
        for _ in 0..3 {
            assert_eq!(sender.check_ffe_submission(), Status::Ready);
        }
        assert_eq!(
            sender.try_submit_ffe(|| panic!("shed before copy")),
            Status::LoadShed
        );
    }
    assert_eq!(sender.try_submit_ffe(|| Ok(request())), Status::Accepted);
    assert!(matches!(
        receive(&peer),
        SidecarInterfaceRequest::EnqueueActions { .. }
    ));
    assert_eq!(sender.channel.0.outstanding(), 21);
    for _ in 0..10 {
        assert_eq!(
            sender.try_submit_ffe(|| panic!("cap before copy")),
            Status::QueueFull
        );
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn ffe_pending_configuration_recovers_or_reports_macos_connection_closure() {
    let (mut sender, peer) = pair();
    full_socket(&sender, &peer);
    sender.set_session_default_service_name(Some("configured".into()));
    let status = sender.try_submit_ffe(|| panic!("priority before copy"));
    match status {
        Status::PriorityPending => {
            // Recoverable congestion: verify priority ordering after draining.
            let mut buf = vec![0; libdd_ipc::max_message_size()];
            while peer.try_recv_raw(&mut buf).is_ok() {}
            assert_eq!(sender.try_submit_ffe(|| Ok(request())), Status::Accepted);
            assert!(
                matches!(receive(&peer), SidecarInterfaceRequest::SetSessionDefaultServiceName { name: Some(name) } if name == "configured")
            );
            assert!(matches!(
                receive(&peer),
                SidecarInterfaceRequest::EnqueueActions { .. }
            ));
        }
        #[cfg(target_os = "macos")]
        Status::Unavailable => {
            // ENOBUFS can close macOS's datagram transport. This branch verifies
            // rejection before capture, not ordering or lifecycle recovery.
            assert!(sender.channel.0.is_closed());
            assert_eq!(sender.check_ffe_submission(), Status::Unavailable);
            assert_eq!(
                sender.try_submit_ffe(|| panic!("closed before copy")),
                Status::Unavailable
            );
        }
        other => panic!("unexpected pending-configuration congestion outcome: {other:?}"),
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn ffe_post_check_congestion_recovers_or_reports_macos_connection_closure() {
    let (mut sender, peer) = pair();
    assert_eq!(sender.check_ffe_submission(), Status::Ready);
    full_socket(&sender, &peer);
    let status = sender.try_submit_ffe(|| Ok(request()));
    assert_eq!(sender.channel.0.outstanding(), 0);
    match status {
        Status::WouldBlock => {
            // Recoverable congestion: a later observation can use this socket.
            assert!(!sender.channel.0.is_closed());
            let mut buf = vec![0; libdd_ipc::max_message_size()];
            while peer.try_recv_raw(&mut buf).is_ok() {}
            assert_eq!(sender.try_submit_ffe(|| Ok(request())), Status::Accepted);
            receive(&peer);
        }
        #[cfg(target_os = "macos")]
        Status::Unavailable => {
            // Closed transport: FFE must not capture again or reconnect here.
            // Successful recovery via the PHP lifecycle is a separate check.
            assert!(sender.channel.0.is_closed());
            assert_eq!(sender.check_ffe_submission(), Status::Unavailable);
            assert_eq!(
                sender.try_submit_ffe(|| panic!("closed before copy")),
                Status::Unavailable
            );
        }
        other => panic!("unexpected post-check congestion outcome: {other:?}"),
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn ffe_closed_connection_stays_closed_without_building_later_observations() {
    let (mut sender, peer) = pair();
    drop(peer);
    assert_eq!(sender.try_submit_ffe(|| Ok(request())), Status::Unavailable);
    assert_eq!(sender.check_ffe_submission(), Status::Unavailable);
    assert_eq!(
        sender.try_submit_ffe(|| panic!("closed before copy")),
        Status::Unavailable
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn ffe_oversize_rejection_preserves_connection_for_next_observation() {
    let (mut sender, peer) = pair();
    let mut oversized = request();
    if let SidecarInterfaceRequest::EnqueueActions { instance_id, .. } = &mut oversized {
        instance_id.runtime_id = "x".repeat(libdd_ipc::max_message_size());
    }
    assert_eq!(
        sender.try_submit_ffe(|| Ok(oversized)),
        Status::PayloadTooLarge
    );
    assert_eq!(sender.channel.0.outstanding(), 0);
    assert_eq!(sender.try_submit_ffe(|| Ok(request())), Status::Accepted);
    receive(&peer);
}

#[test]
#[cfg_attr(miri, ignore)]
fn ffe_submission_does_not_admit_other_actions_or_batches() {
    let (mut sender, _peer) = pair();
    assert_eq!(
        sender.try_submit_ffe(
            || Ok(SidecarInterfaceRequest::SetSessionDefaultServiceName { name: None })
        ),
        Status::InvalidInput
    );
    let mut batch = request();
    if let SidecarInterfaceRequest::EnqueueActions { actions, .. } = &mut batch {
        if let SidecarAction::FfeFlagEvaluationBatch(batch) = &mut actions[0] {
            batch
                .flag_evaluations
                .push(batch.flag_evaluations[0].clone());
        }
    }
    assert_eq!(sender.try_submit_ffe(|| Ok(batch)), Status::InvalidInput);
    assert_eq!(sender.channel.0.outstanding(), 0);
}
