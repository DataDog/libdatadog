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

// Send real ordinary requests and drain the peer without ACKing, keeping the
// shared outstanding count high without conflating it with socket capacity.
fn outstanding(sender: &mut SidecarSender, peer: &SeqpacketConn, count: u64) {
    for _ in 0..count {
        assert!(sender.channel.0.try_send(codec::encode(&request()), &[]));
        receive(peer);
    }
}

fn full_socket(sender: &SidecarSender) {
    sender.channel.0.conn.set_sndbuf_size(4096).unwrap();
    for _ in 0..10_000 {
        match sender.channel.0.conn.try_send_raw(vec![0; 1024], &[]) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
            Err(e) => panic!("unexpected fill failure: {e}"),
        }
    }
    panic!("socket never became full");
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
fn ffe_pending_configuration_precedes_observation_after_backpressure() {
    let (mut sender, peer) = pair();
    full_socket(&sender);
    sender.set_session_default_service_name(Some("configured".into()));
    assert_eq!(
        sender.try_submit_ffe(|| panic!("priority before copy")),
        Status::PriorityPending
    );
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

#[test]
#[cfg_attr(miri, ignore)]
fn ffe_post_check_backpressure_drops_without_closing_connection() {
    let (mut sender, peer) = pair();
    assert_eq!(sender.check_ffe_submission(), Status::Ready);
    full_socket(&sender);
    assert_eq!(sender.try_submit_ffe(|| Ok(request())), Status::WouldBlock);
    assert_eq!(sender.channel.0.outstanding(), 0);
    assert!(!sender.channel.0.is_closed());
    let mut buf = vec![0; libdd_ipc::max_message_size()];
    while peer.try_recv_raw(&mut buf).is_ok() {}
    assert_eq!(sender.try_submit_ffe(|| Ok(request())), Status::Accepted);
    receive(&peer);
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
