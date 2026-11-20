// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::service::{
    ExposureVerdict, ExposureVerdict::*, sidecar_interface::SidecarInterfaceRequest,
};
use libdd_ipc::codec::{decode, encode};
use std::sync::Barrier;
use std::time::{Duration, Instant};

fn client(server: SidecarServer) -> SidecarSender {
    let (local, remote) = SeqpacketConn::socketpair().unwrap();
    let mut handler = ConnectionSidecarHandler::new(server, OwnedServerConn::new(remote).unwrap());
    configure_session(&handler, InstanceId::new("session", "runtime"), |s| {
        set_trace_endpoint(s, "http://localhost:8080/".to_owned());
    });
    tokio::spawn(async move { serve_sidecar_interface_connection(&mut handler).await });
    SidecarSender::new(SidecarInterfaceChannel::new(local))
}

fn record(sender: &mut SidecarSender, event: FfeExposure, deliver: bool) -> ExposureVerdict {
    sender.admit_ffe_exposure_v1(
        ffe_context(),
        event,
        deliver,
        Instant::now() + Duration::from_secs(2),
    )
}

fn authority(
    limit: usize,
    capacity: usize,
) -> (
    SidecarServer,
    tokio::sync::mpsc::Receiver<ffe_exposures_flusher::Job>,
) {
    let (queue, rx) = ffe_exposures_flusher::AdmissionQueue::capture(capacity);
    (
        SidecarServer {
            ffe_exposure_deduplicator: ffe_exposures_flusher::ExposureDeduplicator::new(limit),
            ffe_hook_exposure_deduplicator: Arc::new(std::sync::OnceLock::from(
                ffe_exposures_flusher::ExposureDeduplicator::new(limit),
            )),
            ffe_admission_queue: queue,
            ..SidecarServer::default()
        },
        rx,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
async fn assignment_cycles() {
    for field in ["allocation", "variant", "serial"] {
        let (server, mut rx) = authority(4, 4);
        let (mut w1, mut w2) = (client(server.clone()), client(server));
        let a = ffe_exposure("user");
        let mut b = a.clone();
        match field {
            "allocation" => b.allocation_key = "other".to_owned(),
            "variant" => b.variant = "other".to_owned(),
            _ => b.serial_id = Some(0),
        }
        let verdicts = [
            record(&mut w1, a.clone(), true),
            record(&mut w2, b.clone(), true),
            record(&mut w1, a.clone(), true),
            record(&mut w2, a.clone(), true),
        ];
        assert_eq!(verdicts, [Admitted, Admitted, Admitted, Repeat], "{field}");
        for expected in [a.clone(), b, a] {
            let (_, batch, _) = rx.try_recv().unwrap();
            assert_eq!(batch.exposures.len(), 1);
            let event = &batch.exposures[0];
            assert_eq!(
                (&event.allocation_key, &event.variant, event.serial_id),
                (
                    &expected.allocation_key,
                    &expected.variant,
                    expected.serial_id
                )
            );
        }
        assert!(rx.try_recv().is_err());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
async fn concurrent_first_observations() {
    let (server, mut rx) = authority(1, 8);
    let barrier = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let mut worker = client(server.clone());
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                record(&mut worker, ffe_exposure("same"), true)
            })
        })
        .collect();
    let votes: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(votes.iter().filter(|v| **v == Admitted).count(), 1);
    assert_eq!(votes.iter().filter(|v| **v == Repeat).count(), 7);
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
async fn logging_off_and_eviction() {
    let (server, mut rx) = authority(1, 1);
    let mut worker = client(server);
    for (user, verdict) in [
        ("one", Admitted),
        ("one", Repeat),
        ("two", Admitted),
        ("one", Admitted),
    ] {
        assert_eq!(record(&mut worker, ffe_exposure(user), false), verdict);
    }
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
async fn hook_only_observations_preserve_delivery_and_its_capacity() {
    let (server, mut rx) = authority(1, 1);
    let mut cli = client(server.clone());
    let mut fpm = client(server);
    let a = ffe_exposure("a");
    assert_eq!(record(&mut cli, a.clone(), false), Admitted);
    assert_eq!(record(&mut fpm, a.clone(), false), Repeat);
    assert!(rx.try_recv().is_err());
    assert_eq!(record(&mut fpm, a.clone(), true), Admitted);
    assert_eq!(record(&mut cli, ffe_exposure("b"), false), Admitted);
    assert_eq!(record(&mut fpm, a, true), Repeat);
    assert_eq!(rx.try_recv().unwrap().1.exposures[0].subject_id, "a");
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
async fn refusal_stats_distinguish_the_failure_phase() {
    let (server, rx) = authority(1, 1);
    let mut handler = test_handler(server.clone());
    assert_eq!(
        handler
            .admit_ffe_exposure_v1(ffe_context(), ffe_exposure("no-route"), true)
            .await,
        Unavailable
    );
    let mut worker = client(server.clone());
    let mut bad = ffe_exposure("invalid");
    bad.variant.clear();
    assert_eq!(record(&mut worker, bad, true), Unavailable);
    assert_eq!(record(&mut worker, ffe_exposure("first"), true), Admitted);
    assert_eq!(record(&mut worker, ffe_exposure("next"), true), QueueFull);
    drop(rx);
    assert_eq!(
        record(&mut worker, ffe_exposure("stopped"), true),
        Unavailable
    );
    let stats = server.compute_stats().await.ffe_exposures;
    assert_eq!(
        (
            stats.missing_transport,
            stats.invalid,
            stats.full,
            stats.closed
        ),
        (1, 1, 1, 1)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
async fn queue_full_preserves_assignment() {
    let (server, mut rx) = authority(4, 1);
    let mut worker = client(server);
    let a = ffe_exposure("user");
    let mut b = a.clone();
    b.variant = "b".to_owned();
    assert_eq!(record(&mut worker, a.clone(), true), Admitted);
    assert_eq!(record(&mut worker, b.clone(), true), QueueFull);
    assert_eq!(record(&mut worker, a, true), Repeat);
    assert_eq!(rx.try_recv().unwrap().1.exposures[0].variant, "variant");
    assert_eq!(record(&mut worker, b, true), Admitted);
    assert_eq!(rx.try_recv().unwrap().1.exposures[0].variant, "b");
}

#[test]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
fn admission_drains_pending_state_and_consumes_prior_acks() {
    let (conn, peer) = SeqpacketConn::socketpair().unwrap();
    let mut sender = SidecarSender::new(SidecarInterfaceChannel::new(conn));
    sender.max_outstanding = 1;
    sender.set_user_service_defined(true);
    sender.set_test_session_token("new-scope".to_owned());
    assert_eq!(
        record(&mut sender, ffe_exposure("user"), false),
        Unavailable
    );
    sender.max_outstanding = 100;

    let server = std::thread::spawn(move || {
        let mut buf = [0; 4096];
        let mut read = || {
            let (len, _) = peer.recv_raw_blocking(&mut buf).unwrap();
            decode::<SidecarInterfaceRequest>(&buf[..len]).unwrap()
        };
        assert!(matches!(
            read(),
            SidecarInterfaceRequest::SetUserServiceDefined { is_defined: true }
        ));
        assert!(
            matches!(read(), SidecarInterfaceRequest::SetTestSessionToken { token } if token == "new-scope")
        );
        assert!(matches!(read(), SidecarInterfaceRequest::Ping {}));
        for byte in [0, 0, crate::service::FFE_ADMISSION_V1_CAPABILITY] {
            peer.send_raw_blocking(vec![byte], &[]).unwrap();
        }
        match read() {
            SidecarInterfaceRequest::AdmitFfeExposureV1 {
                event,
                deliver: false,
                ..
            } => {
                assert!(event.subject_attributes_json.is_empty());
                assert!(event.features_json.is_empty());
            }
            _ => panic!("expected hook-only admission"),
        }
        peer.send_raw_blocking(encode(&Admitted), &[]).unwrap();
    });
    let mut event = ffe_exposure("user");
    event.subject_attributes_json = r#"{"id":1}"#.to_owned();
    event.features_json = r#"{"arm":"test"}"#.to_owned();
    assert_eq!(record(&mut sender, event, false), Admitted);
    assert_eq!(sender.channel.0.outstanding(), 0);
    server.join().unwrap();
}

fn simulated_peer(
    supported: bool,
    after_probe: impl FnOnce(SeqpacketConn) + Send + 'static,
) -> (SidecarSender, std::thread::JoinHandle<()>) {
    simulated_peer_with_caps(u8::from(supported), after_probe)
}

fn simulated_peer_with_caps(
    caps: u8,
    after_probe: impl FnOnce(SeqpacketConn) + Send + 'static,
) -> (SidecarSender, std::thread::JoinHandle<()>) {
    let (client, peer) = SeqpacketConn::socketpair().unwrap();
    let handle = std::thread::spawn(move || {
        let mut buf = [0; 4096];
        let (len, _) = peer.recv_raw_blocking(&mut buf).unwrap();
        assert!(matches!(
            decode::<SidecarInterfaceRequest>(&buf[..len]).unwrap(),
            SidecarInterfaceRequest::Ping {}
        ));
        peer.send_raw_blocking(vec![caps], &[]).unwrap();
        after_probe(peer);
    });
    (
        SidecarSender::new(SidecarInterfaceChannel::new(client)),
        handle,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
async fn exact_peer_waits_for_the_checked_prefix_and_applied_repeat() {
    use crate::service::ffe_deferred::ChunkOutcome;
    use std::sync::mpsc::{RecvTimeoutError, channel};

    let (server, mut queued) = authority(2, 2);
    let mut upstream = client(server);
    let (worker, relay) = SeqpacketConn::socketpair().unwrap();
    let (applied, observed) = channel();
    let (release, gate) = channel();
    let (prefix, prefix_result) = channel();
    let (complete, result) = channel();
    let peer = std::thread::spawn(move || {
        let mut buf = [0; 4096];
        for deferred in [true, false] {
            let (n, _) = relay.recv_raw_blocking(&mut buf).unwrap();
            assert!(matches!(
                decode::<SidecarInterfaceRequest>(&buf[..n]).unwrap(),
                SidecarInterfaceRequest::Ping {}
            ));
            relay
                .send_raw_blocking(encode(&upstream.channel.call_ping().unwrap()), &[])
                .unwrap();
            let (n, _) = relay.recv_raw_blocking(&mut buf).unwrap();
            let request = decode::<SidecarInterfaceRequest>(&buf[..n]).unwrap();
            let reply = if deferred {
                assert!(
                    matches!(&request, SidecarInterfaceRequest::SubmitDeferredFfeV1 { batch } if batch.exposures.len() == 1 && batch.exposures[0].subject_id == "same")
                );
                let accepted: bool = upstream.channel.call_request_blocking(&request).unwrap();
                assert!(accepted);
                encode(&accepted)
            } else {
                assert!(
                    matches!(&request, SidecarInterfaceRequest::AdmitFfeExposureV1 { event, deliver: true, .. } if event.subject_id == "same")
                );
                let reply: ExposureVerdict =
                    upstream.channel.call_request_blocking(&request).unwrap();
                assert_eq!(reply, Repeat);
                encode(&reply)
            };
            applied.send(()).unwrap();
            gate.recv_timeout(Duration::from_secs(10)).unwrap();
            relay.send_raw_blocking(reply, &[]).unwrap();
        }
    });
    let worker = std::thread::spawn(move || {
        let mut sender = SidecarSender::new(SidecarInterfaceChannel::new(worker));
        prefix
            .send(sender.submit_deferred_chunk_for_test(
                ffe_context(),
                std::iter::once(Some(ffe_exposure("same"))),
            ))
            .unwrap();
        complete
            .send(sender.admit_ffe_exposure_for_test(ffe_context(), ffe_exposure("same"), true))
            .unwrap();
    });
    observed.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(
        prefix_result.recv_timeout(Duration::from_millis(100)),
        Err(RecvTimeoutError::Timeout),
        "prefix completed before its acknowledgement"
    );
    release.send(()).unwrap();
    assert_eq!(
        prefix_result.recv_timeout(Duration::from_secs(10)).unwrap(),
        (ChunkOutcome::Confirmed, 1)
    );
    observed.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(
        result.recv_timeout(Duration::from_millis(100)),
        Err(RecvTimeoutError::Timeout)
    );
    release.send(()).unwrap();
    assert_eq!(
        result.recv_timeout(Duration::from_secs(10)).unwrap(),
        Repeat
    );
    worker.join().unwrap();
    peer.join().unwrap();
    assert_eq!(queued.try_recv().unwrap().1.exposures.len(), 1);
    assert!(queued.try_recv().is_err());
}

fn submit(
    sender: &mut SidecarSender,
    rows: &[FfeExposure],
) -> (crate::service::ffe_deferred::ChunkOutcome, u32) {
    sender.submit_deferred_chunk(
        ffe_context(),
        rows.iter().cloned().map(Some),
        Instant::now() + Duration::from_secs(5),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(
    miri,
    ignore = "requires native sequenced-packet sockets and shared memory"
)]
async fn wire_sized_chunks_remain_ordered_while_admission_is_full() {
    use crate::service::ffe_deferred::ChunkOutcome::Confirmed;
    let (server, mut rx) = authority(8, 1);
    let mut sender = client(server);
    assert_eq!(record(&mut sender, ffe_exposure("seed"), true), Admitted);
    let mut rows: Vec<_> = (0..66)
        .map(|i| {
            let mut row = ffe_exposure("subject");
            row.subject_attributes_json = format!(r#"{{"blob":"{}"}}"#, "x".repeat(70 * 1024));
            row.variant = i.to_string();
            row
        })
        .collect();
    rows.push(ffe_exposure("sentinel"));
    let mut n = 0;
    let mut chunks = 0;
    while n < rows.len() {
        let (outcome, count) = submit(&mut sender, &rows[n..]);
        assert_eq!(outcome, Confirmed);
        assert!(count > 0);
        n += count as usize;
        chunks += 1;
    }
    assert!(chunks >= 2);
    let _seed = rx.try_recv().unwrap();
    let mut observed = Vec::new();
    while let Ok((_, batch, _)) = rx.try_recv() {
        observed.extend(batch.exposures);
    }
    assert_eq!(observed, rows);
    assert_eq!(record(&mut sender, ffe_exposure("sentinel"), true), Repeat);
}

#[test]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
fn deferred_outcome_tracks_only_the_attempted_prefix() {
    use crate::service::ffe_deferred::ChunkOutcome::*;
    for (caps, reply, expected) in [
        (0, Some(false), (LegacySubmitted, 2)),
        (1, Some(false), (LegacySubmitted, 2)),
        (3, Some(false), (Refused, 0)),
        (3, None, (Unknown, 2)),
    ] {
        let (mut sender, peer) = simulated_peer_with_caps(caps, move |peer| {
            let mut buf = [0; 4096];
            let (len, _) = peer.recv_raw_blocking(&mut buf).unwrap();
            match decode::<SidecarInterfaceRequest>(&buf[..len]).unwrap() {
                SidecarInterfaceRequest::EnqueueActions { actions } if caps < 2 => assert!(
                    matches!(actions.as_slice(), [SidecarAction::FfeExposureBatch(b)] if b.exposures.len() == 2)
                ),
                SidecarInterfaceRequest::SubmitDeferredFfeV1 { batch } if caps == 3 => {
                    assert_eq!(batch.exposures.len(), 2);
                    if let Some(ok) = reply {
                        peer.send_raw_blocking(encode(&ok), &[]).unwrap();
                    }
                }
                _ => panic!("unexpected deferred wire opcode"),
            }
        });
        assert_eq!(
            sender.supports_ordered_ffe_handoff_until(Instant::now() + Duration::from_secs(5)),
            caps == 3
        );
        assert_eq!(
            submit(&mut sender, &[ffe_exposure("a"), ffe_exposure("b")]),
            expected
        );
        peer.join().unwrap();
    }
}

#[test]
#[cfg_attr(miri, ignore = "creates a native shared-memory handle")]
fn legacy_oversized_opcode() {
    let envelope = SidecarInterfaceRequest::_Oversized {
        shm: libdd_ipc::platform::ShmHandle::new(4096).unwrap(),
        len: 1,
    };
    assert_eq!(encode(&envelope)[..4], 31u32.to_le_bytes());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
async fn admitted_event_reaches_http() {
    let http = MockServer::start_async().await;
    let mock = http
        .mock_async(|when, then| {
            when.method(POST)
                .path(ffe_exposures_flusher::EVP_EXPOSURES_PATH);
            then.status(202);
        })
        .await;
    let mut handler = test_handler(SidecarServer::default());
    configure_session(&handler, InstanceId::new("session", "runtime"), |s| {
        set_trace_endpoint(s, http.url("/"))
    });
    assert_eq!(
        handler
            .admit_ffe_exposure_v1(ffe_context(), ffe_exposure("user"), true)
            .await,
        Admitted
    );
    assert_eq!(
        handler
            .admit_ffe_exposure_v1(ffe_context(), ffe_exposure("user"), true)
            .await,
        Repeat
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while mock.calls_async().await == 0 {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    mock.assert_async().await;
}

#[test]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
fn old_peer_returns_unavailable() {
    let (done, wait) = std::sync::mpsc::channel();
    let (mut sender, peer) = simulated_peer(false, move |peer| {
        wait.recv_timeout(Duration::from_secs(4)).unwrap();
        let mut buf = [0; 4096];
        assert_eq!(
            peer.try_recv_raw(&mut buf).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    });
    assert_eq!(record(&mut sender, ffe_exposure("user"), true), Unavailable);
    done.send(()).unwrap();
    peer.join().unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
fn late_capability_reply_preserves_transport() {
    let (conn, peer) = SeqpacketConn::socketpair().unwrap();
    let mut sender = SidecarSender::new(SidecarInterfaceChannel::new(conn));
    let (release, hold) = std::sync::mpsc::channel();
    let (sent, arrived) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut buf = [0; 4096];
        for round in 0..2 {
            let (len, _) = peer.recv_raw_blocking(&mut buf).unwrap();
            assert!(matches!(
                decode::<SidecarInterfaceRequest>(&buf[..len]).unwrap(),
                SidecarInterfaceRequest::Ping {}
            ));
            if round == 0 {
                hold.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            peer.send_raw_blocking(vec![1], &[]).unwrap();
            if round == 0 {
                sent.send(()).unwrap();
            }
        }
        let (len, _) = peer.recv_raw_blocking(&mut buf).unwrap();
        assert!(matches!(
            decode::<SidecarInterfaceRequest>(&buf[..len]).unwrap(),
            SidecarInterfaceRequest::AdmitFfeExposureV1 { .. }
        ));
        peer.send_raw_blocking(encode(&Admitted), &[]).unwrap();
    });
    assert_eq!(
        sender.admit_ffe_exposure_v1(
            ffe_context(),
            ffe_exposure("cold"),
            true,
            Instant::now() + Duration::from_millis(10)
        ),
        Unavailable
    );
    assert!(!sender.channel.0.is_closed());
    assert_eq!(
        record(&mut sender, ffe_exposure("waiting"), true),
        Unavailable
    );
    release.send(()).unwrap();
    arrived.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(record(&mut sender, ffe_exposure("ready"), true), Admitted);
    server.join().unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
fn lost_reply_is_unknown() {
    let (mut sender, peer) = simulated_peer(true, |peer| {
        let mut buf = [0; 4096];
        let (len, _) = peer.recv_raw_blocking(&mut buf).unwrap();
        assert!(matches!(
            decode::<SidecarInterfaceRequest>(&buf[..len]).unwrap(),
            SidecarInterfaceRequest::AdmitFfeExposureV1 { .. }
        ));
    });
    assert_eq!(record(&mut sender, ffe_exposure("user"), true), Unknown);
    assert_eq!(record(&mut sender, ffe_exposure("next"), true), Unavailable);
    peer.join().unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "requires native sequenced-packet sockets")]
fn stalled_peer_respects_deadline() {
    let (release, hold) = std::sync::mpsc::channel::<()>();
    let (sent, arrived) = std::sync::mpsc::channel();
    let (mut sender, peer) = simulated_peer(true, move |peer| {
        let mut buf = [0; 4096];
        peer.recv_raw_blocking(&mut buf).unwrap();
        peer.send_raw_blocking(encode(&Admitted), &[]).unwrap();
        hold.recv_timeout(Duration::from_secs(2)).unwrap();
        peer.recv_raw_blocking(&mut buf).unwrap();
        peer.send_raw_blocking(encode(&Repeat), &[]).unwrap();
        sent.send(()).unwrap();
        let (len, _) = peer.recv_raw_blocking(&mut buf).unwrap();
        assert!(
            matches!(decode::<SidecarInterfaceRequest>(&buf[..len]).unwrap(), SidecarInterfaceRequest::AdmitFfeExposureV1 { event, .. } if event.subject_id == "fresh")
        );
        peer.send_raw_blocking(encode(&Admitted), &[]).unwrap();
    });
    assert_eq!(record(&mut sender, ffe_exposure("warm"), false), Admitted);
    let start = Instant::now();
    assert_eq!(
        sender.admit_ffe_exposure_v1(
            ffe_context(),
            ffe_exposure("user"),
            true,
            start + Duration::from_millis(10)
        ),
        Unknown
    );
    assert!(start.elapsed() < Duration::from_millis(100));
    assert_eq!(
        record(&mut sender, ffe_exposure("waiting"), true),
        Unavailable
    );
    release.send(()).unwrap();
    arrived.recv_timeout(Duration::from_secs(2)).unwrap();
    sender.channel.0.drain_acks();
    assert_eq!(record(&mut sender, ffe_exposure("fresh"), true), Admitted);
    peer.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "local IPC latency measurement"]
async fn local_admission_latency() {
    const SOCKET: &str = "_DD_TEST_ADMISSION_BENCH_SOCKET";
    let Some(path) = std::env::var_os(SOCKET) else {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bench.sock");
        let listener = libdd_ipc::SeqpacketListener::bind(&path).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "service::sidecar_server::tests::admission::local_admission_latency",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env(SOCKET, path)
            .spawn()
            .unwrap();
        let connection = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match listener.try_accept() {
                    Ok(conn) => break conn,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        sleep(Duration::from_millis(1)).await
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            }
        })
        .await
        .unwrap();
        let (server, mut rx) = authority(65536, 128);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let mut handler =
            ConnectionSidecarHandler::new(server, OwnedServerConn::new(connection).unwrap());
        configure_session(&handler, InstanceId::new("bench", "runtime"), |session| {
            set_trace_endpoint(session, "http://localhost:8080/".to_owned())
        });
        tokio::spawn(async move { serve_sidecar_interface_connection(&mut handler).await });
        assert!(child.wait().unwrap().success());
        return;
    };
    let mut worker = SidecarSender::new(SidecarInterfaceChannel::new(
        SeqpacketConn::connect(path).unwrap(),
    ));
    sample_latency(&mut worker);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "local IPC latency measurement"]
async fn thread_admission_latency() {
    let (server, mut rx) = authority(65536, 128);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    sample_latency(&mut client(server));
}

fn sample_latency(worker: &mut SidecarSender) {
    assert_eq!(record(worker, ffe_exposure("warm"), true), Admitted);
    for repeats in [true, false] {
        let mut durations = Vec::with_capacity(10000);
        for i in 0..10000 {
            let start = Instant::now();
            let key = if repeats {
                "warm".to_owned()
            } else {
                i.to_string()
            };
            let status = record(worker, ffe_exposure(&key), true);
            durations.push(start.elapsed());
            assert_eq!(status, if repeats { Repeat } else { Admitted });
        }
        durations.sort_unstable();
        eprintln!(
            "repeat={repeats} n=10000 p50={:?} p99={:?} p999={:?}",
            durations[4999], durations[9899], durations[9989]
        );
    }
}
