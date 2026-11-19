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
    tokio::sync::mpsc::Receiver<(
        crate::service::evp_transport::EvpTransport,
        FfeExposureBatch,
    )>,
) {
    let (queue, rx) = ffe_exposures_flusher::AdmissionQueue::capture(capacity);
    (
        SidecarServer {
            ffe_exposure_deduplicator: ffe_exposures_flusher::ExposureDeduplicator::new(limit),
            ffe_admission_queue: queue,
            ..SidecarServer::default()
        },
        rx,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
            let (_, batch) = rx.try_recv().unwrap();
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
async fn queue_full_preserves_assignment() {
    let (server, mut rx) = authority(4, 1);
    let mut worker = client(server);
    let a = ffe_exposure("user");
    let mut b = a.clone();
    b.variant = "b".to_owned();
    assert_eq!(record(&mut worker, a.clone(), true), Admitted);
    assert_eq!(record(&mut worker, b.clone(), true), Unavailable);
    assert_eq!(record(&mut worker, a, true), Repeat);
    assert_eq!(rx.try_recv().unwrap().1.exposures[0].variant, "variant");
    assert_eq!(record(&mut worker, b, true), Admitted);
    assert_eq!(rx.try_recv().unwrap().1.exposures[0].variant, "b");
}

fn simulated_peer(
    supported: bool,
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
        peer.send_raw_blocking(vec![u8::from(supported)], &[])
            .unwrap();
        after_probe(peer);
    });
    (
        SidecarSender::new(SidecarInterfaceChannel::new(client)),
        handle,
    )
}

#[test]
fn legacy_oversized_opcode() {
    let envelope = SidecarInterfaceRequest::_Oversized {
        shm: libdd_ipc::platform::ShmHandle::new(4096).unwrap(),
        len: 1,
    };
    assert_eq!(encode(&envelope)[..4], 31u32.to_le_bytes());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    assert!(sender.channel.0.is_closed());
    peer.join().unwrap();
}

#[test]
fn stalled_peer_respects_deadline() {
    let (release, hold) = std::sync::mpsc::channel::<()>();
    let (mut sender, peer) = simulated_peer(true, move |peer| {
        let mut buf = [0; 4096];
        peer.recv_raw_blocking(&mut buf).unwrap();
        peer.send_raw_blocking(encode(&Admitted), &[]).unwrap();
        let _ = hold.recv_timeout(Duration::from_secs(2));
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
    assert!(sender.channel.0.is_closed());
    drop(release);
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
