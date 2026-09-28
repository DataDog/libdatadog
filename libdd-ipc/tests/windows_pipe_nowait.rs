// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(windows)]

//! Behaviour of both ends of the Windows named-pipe transport, which are kept in `PIPE_NOWAIT`
//! mode for their whole lifetime and block by waiting on the NPFS readiness event.
//!
//! Most tests run once per end ([`End`]): the end under test is exercised, the other end of the
//! socketpair acts as the peer.  The server end is an overlapped pipe instance (created with
//! `FILE_FLAG_OVERLAPPED` for the listener's cancellable accept), the client end a synchronous
//! handle, so both kinds of handle are covered.

use std::io;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use libdd_ipc::{max_message_size, SeqpacketConn, SeqpacketListener, HANDLE_SUFFIX_SIZE};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Pipes::{
    GetNamedPipeHandleStateW, PIPE_NOWAIT, PIPE_READMODE_MESSAGE,
};

/// The end of a socketpair a test exercises; the other end is the peer.
#[derive(Clone, Copy, Debug)]
enum End {
    Client,
    Server,
}

/// A connected socketpair as `(end under test, peer)`.
fn pair(end: End) -> (SeqpacketConn, SeqpacketConn) {
    let (server, client) = SeqpacketConn::socketpair().unwrap();
    match end {
        End::Client => (client, server),
        End::Server => (server, client),
    }
}

/// Messages used to fill the pipe; small relative to the default 4 MiB pipe buffer.
const FILL_PAYLOAD: usize = 64 * 1024;
/// Upper bound for "promptly" (generous, to absorb VM scheduling noise, but far below any
/// timeout used in these tests).
const PROMPT: Duration = Duration::from_millis(250);

/// Run `f` on a separate thread and fail the test instead of hanging forever.
fn with_timeout<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(limit) {
        Ok(v) => v,
        Err(RecvTimeoutError::Timeout) => panic!("test body did not finish within {limit:?}"),
        Err(RecvTimeoutError::Disconnected) => panic!("test body panicked"),
    }
}

fn pipe_state(conn: &SeqpacketConn) -> u32 {
    pipe_handle_state(conn.as_raw_handle() as HANDLE).0
}

/// `(state, current instance count)` of a named pipe handle.
fn pipe_handle_state(handle: HANDLE) -> (u32, u32) {
    let mut state: u32 = 0;
    let mut instances: u32 = 0;
    let ok = unsafe {
        GetNamedPipeHandleStateW(
            handle,
            &mut state,
            &mut instances,
            null_mut(),
            null_mut(),
            null_mut(),
            0,
        )
    };
    assert_ne!(
        ok,
        0,
        "GetNamedPipeHandleStateW failed: {}",
        io::Error::last_os_error()
    );
    (state, instances)
}

#[track_caller]
fn assert_nowait(conn: &SeqpacketConn, when: &str) {
    let state = pipe_state(conn);
    assert_eq!(
        state & (PIPE_NOWAIT | PIPE_READMODE_MESSAGE),
        PIPE_NOWAIT | PIPE_READMODE_MESSAGE,
        "pipe state {state:#x} is not PIPE_NOWAIT|PIPE_READMODE_MESSAGE {when}"
    );
}

fn msg(seq: u32, len: usize) -> Vec<u8> {
    let mut v = vec![0xA5u8; len.max(4)];
    v[..4].copy_from_slice(&seq.to_le_bytes());
    v
}

fn seq_of(buf: &[u8]) -> u32 {
    u32::from_le_bytes(buf[..4].try_into().unwrap())
}

fn recv_buf() -> Vec<u8> {
    vec![0u8; max_message_size() + HANDLE_SUFFIX_SIZE]
}

#[track_caller]
fn assert_kind<T: std::fmt::Debug>(r: io::Result<T>, kind: io::ErrorKind, what: &str) {
    match r {
        Err(e) => assert_eq!(e.kind(), kind, "{what}: unexpected error {e:?}"),
        Ok(v) => panic!("{what}: expected {kind:?}, got Ok({v:?})"),
    }
}

/// Fill the `sender` -> peer direction with `FILL_PAYLOAD`-sized messages until `try_send_raw`
/// reports `WouldBlock`.  Returns the number of messages accepted (sequence numbers `0..n`).
fn fill(sender: &SeqpacketConn) -> u32 {
    let mut sent = 0u32;
    loop {
        match sender.try_send_raw(&mut msg(sent, FILL_PAYLOAD), &[]) {
            Ok(()) => sent += 1,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) => panic!("try_send_raw failed while filling: {e:?}"),
        }
        assert!(sent < 100_000, "pipe never filled up");
    }
    assert_nowait(sender, "after try_send_raw returned WouldBlock");
    sent
}

/// Receive exactly `count` fill messages on `receiver` and check that they arrive in order.
fn drain(receiver: &SeqpacketConn, first_seq: u32, count: u32, buf: &mut [u8]) {
    for expected in first_seq..first_seq + count {
        let (n, handles) = receiver.recv_raw_blocking(buf).expect("recv");
        assert!(handles.is_empty());
        assert_eq!(n, FILL_PAYLOAD, "message {expected} has the wrong length");
        assert_eq!(
            seq_of(buf),
            expected,
            "message lost, duplicated or reordered"
        );
    }
}

/// Requirement: the handle is PIPE_NOWAIT from connect / accept on, and no operation (success,
/// WouldBlock, timeout) ever switches it back to PIPE_WAIT.  For the server end this is the bug
/// being fixed: non-blocking server writes used to toggle the handle to PIPE_NOWAIT and back.
fn check_mode_is_nowait_for_entire_lifetime(end: End) {
    with_timeout(Duration::from_secs(60), move || {
        let (mut me, peer) = pair(end);
        let mut pbuf = recv_buf();
        let mut buf = recv_buf();
        assert_nowait(&me, "after connect");

        assert_kind(
            me.try_recv_raw(&mut buf),
            io::ErrorKind::WouldBlock,
            "try_recv_raw on empty pipe",
        );
        assert_nowait(&me, "after try_recv_raw WouldBlock");

        me.try_send_raw(&mut msg(1, 16), &[]).unwrap();
        assert_nowait(&me, "after try_send_raw Ok");
        me.send_raw_blocking(&mut msg(2, 16), &[]).unwrap();
        assert_nowait(&me, "after send_raw_blocking Ok");
        for expected in [1, 2] {
            let (n, _) = peer.recv_raw_blocking(&mut pbuf).unwrap();
            assert_eq!((n, seq_of(&pbuf)), (16, expected));
        }

        peer.send_raw_blocking(&mut msg(3, 16), &[]).unwrap();
        let (n, _) = me.try_recv_raw(&mut buf).unwrap();
        assert_eq!((n, seq_of(&buf)), (16, 3));
        assert_nowait(&me, "after try_recv_raw Ok");

        peer.send_raw_blocking(&mut msg(4, 16), &[]).unwrap();
        let (n, _) = me.recv_raw_blocking(&mut buf).unwrap();
        assert_eq!((n, seq_of(&buf)), (16, 4));
        assert_nowait(&me, "after recv_raw_blocking Ok");

        assert_eq!(me.drain_acks_nonblocking(5).unwrap(), 0);
        assert_nowait(&me, "after drain_acks_nonblocking");

        me.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let t = Instant::now();
        assert_kind(
            me.recv_raw_blocking(&mut buf),
            io::ErrorKind::TimedOut,
            "recv_raw_blocking with read timeout",
        );
        let waited = t.elapsed();
        assert!(
            waited >= Duration::from_millis(90) && waited < Duration::from_secs(2),
            "read timeout took {waited:?}"
        );
        assert_nowait(&me, "after recv_raw_blocking TimedOut");

        let filled = fill(&me);
        me.set_write_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        assert_kind(
            me.send_raw_blocking(&mut msg(filled, FILL_PAYLOAD), &[]),
            io::ErrorKind::TimedOut,
            "send_raw_blocking on a full pipe with write timeout",
        );
        assert_nowait(&me, "after send_raw_blocking TimedOut");

        drain(&peer, 0, filled, &mut pbuf);
        assert_kind(
            peer.try_recv_raw(&mut pbuf),
            io::ErrorKind::WouldBlock,
            "timed-out send must not have been written",
        );
        assert_nowait(&me, "at end of test");
    });
}

#[test]
fn client_mode_is_nowait_for_entire_lifetime() {
    check_mode_is_nowait_for_entire_lifetime(End::Client);
}

#[test]
fn server_mode_is_nowait_for_entire_lifetime() {
    check_mode_is_nowait_for_entire_lifetime(End::Server);
}

fn unique_pipe_path(tag: u32) -> String {
    format!(
        r"\\.\pipe\libdd-ipc-nowait-test-{}-{tag}",
        std::process::id()
    )
}

/// Connecting through a real listener: the PID handshake read (which now waits on the
/// readiness event instead of a PIPE_WAIT ReadFile) completes and yields the acceptor's PID, and
/// the accepted server end is in PIPE_NOWAIT mode before it is handed out.
#[test]
fn listener_connect_handshake_waits_for_pid() {
    with_timeout(Duration::from_secs(60), || {
        let path = unique_pipe_path(line!());
        let listener = SeqpacketListener::bind(&path).unwrap();
        let connect_path = path.clone();
        let client = thread::spawn(move || SeqpacketConn::connect(connect_path));

        // Accept only after a delay: the client must be parked waiting for the handshake.
        thread::sleep(Duration::from_millis(200));
        let server = loop {
            match listener.try_accept() {
                Ok(c) => break c,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("try_accept: {e:?}"),
            }
        };
        let accepted_at = Instant::now();
        assert_nowait(&server, "server end right after try_accept");
        let client = client.join().unwrap().unwrap();
        let handshake_latency = accepted_at.elapsed();
        assert!(
            handshake_latency < PROMPT,
            "connect returned {handshake_latency:?} after the handshake was written"
        );
        assert_eq!(client.peer_credentials().unwrap().pid, std::process::id());
        assert_nowait(&client, "after connect via listener");

        let mut sbuf = recv_buf();
        client.send_raw_blocking(&mut msg(7, 32), &[]).unwrap();
        let (n, _) = server.recv_raw_blocking(&mut sbuf).unwrap();
        assert_eq!((n, seq_of(&sbuf)), (32, 7));

        let mut cbuf = recv_buf();
        server.send_raw_blocking(&mut msg(8, 32), &[]).unwrap();
        let (n, _) = client.recv_raw_blocking(&mut cbuf).unwrap();
        assert_eq!((n, seq_of(&cbuf)), (32, 8));
        assert_nowait(&server, "server end after round trip");
    });
}

/// A blocked receive must wake up as soon as the peer writes, not after a poll interval.
fn check_blocking_recv_wakes_promptly(end: End) {
    with_timeout(Duration::from_secs(60), move || {
        let (me, peer) = pair(end);
        let mut buf = recv_buf();
        let mut worst = Duration::ZERO;
        for round in 0..10u32 {
            let (tx, rx) = mpsc::channel();
            let writer = thread::scope(|s| {
                s.spawn(|| {
                    thread::sleep(Duration::from_millis(150));
                    let written_at = Instant::now();
                    peer.send_raw_blocking(&mut msg(round, 64), &[]).unwrap();
                    tx.send(written_at).unwrap();
                });
                let started = Instant::now();
                let (n, _) = me.recv_raw_blocking(&mut buf).unwrap();
                let returned_at = Instant::now();
                assert_eq!((n, seq_of(&buf)), (64, round));
                assert!(
                    returned_at - started >= Duration::from_millis(100),
                    "recv returned before the peer wrote"
                );
                returned_at
            });
            let written_at = rx.recv().unwrap();
            let latency = writer.saturating_duration_since(written_at);
            worst = worst.max(latency);
        }
        eprintln!("{end:?}: worst recv wake-up latency after peer write: {worst:?}");
        assert!(worst < Duration::from_millis(50), "slow wake-up: {worst:?}");
        assert_nowait(&me, "after blocking receives");
    });
}

#[test]
fn blocking_recv_wakes_promptly() {
    check_blocking_recv_wakes_promptly(End::Client);
}

#[test]
fn server_blocking_recv_wakes_promptly() {
    check_blocking_recv_wakes_promptly(End::Server);
}

/// Many blocking round trips; a lost wake-up would hang (and trip the timeout).  Both ends block.
#[test]
fn blocking_ping_pong_no_lost_wakeups() {
    with_timeout(Duration::from_secs(120), || {
        let (server, client) = SeqpacketConn::socketpair().unwrap();
        const ROUNDS: u32 = 5000;
        thread::scope(|s| {
            s.spawn(|| {
                let mut sbuf = recv_buf();
                for i in 0..ROUNDS {
                    let (n, _) = server.recv_raw_blocking(&mut sbuf).unwrap();
                    assert_eq!(seq_of(&sbuf[..n]), i);
                    server.send_raw_blocking(&mut msg(i, 8), &[]).unwrap();
                }
            });
            let mut cbuf = recv_buf();
            let start = Instant::now();
            for i in 0..ROUNDS {
                client.send_raw_blocking(&mut msg(i, 8), &[]).unwrap();
                let (n, _) = client.recv_raw_blocking(&mut cbuf).unwrap();
                assert_eq!((n, seq_of(&cbuf)), (8, i));
            }
            eprintln!("{ROUNDS} blocking round trips in {:?}", start.elapsed());
        });
        assert_nowait(&client, "client after ping-pong");
        assert_nowait(&server, "server after ping-pong");
    });
}

/// Backpressure: a full pipe yields a real `WouldBlock` (not a silent zero-byte "success"), a
/// blocking send stays blocked until the peer drains, and every message counted as sent is
/// received exactly once, in order.  On the server end the old non-blocking write reported a
/// zero-byte `WriteFile` "success" on a full pipe as a completed send.
fn check_backpressure_without_silent_drops(end: End) {
    with_timeout(Duration::from_secs(120), move || {
        let (me, peer) = pair(end);
        let me = Arc::new(me);
        let mut pbuf = recv_buf();

        for iteration in 0..3 {
            let filled = fill(&me);
            assert!(filled > 0);
            let wire = FILL_PAYLOAD + 4;
            assert!(
                filled as usize * wire <= max_message_size() + HANDLE_SUFFIX_SIZE,
                "{filled} messages of {wire} bytes cannot fit in the pipe buffer: some \
                 'successful' sends must have been dropped"
            );
            // WouldBlock is sticky while nobody reads, and nothing was written by it.
            for _ in 0..3 {
                assert_kind(
                    me.try_send_raw(&mut msg(u32::MAX, FILL_PAYLOAD), &[]),
                    io::ErrorKind::WouldBlock,
                    "try_send_raw on a full pipe",
                );
            }

            let done = Arc::new(AtomicBool::new(false));
            let done_at = Arc::new(AtomicU64::new(0));
            let epoch = Instant::now();
            let writer = {
                let (me, done, done_at) = (me.clone(), done.clone(), done_at.clone());
                thread::spawn(move || {
                    let r = me.send_raw_blocking(&mut msg(filled, FILL_PAYLOAD), &[]);
                    done_at.store(epoch.elapsed().as_micros() as u64, Ordering::SeqCst);
                    done.store(true, Ordering::SeqCst);
                    r
                })
            };
            thread::sleep(Duration::from_millis(300));
            assert!(
                !done.load(Ordering::SeqCst),
                "send_raw_blocking returned while the pipe was full"
            );

            // Draining the first message frees enough room for the blocked writer.
            let drain_started_at = epoch.elapsed();
            drain(&peer, 0, 1, &mut pbuf);
            drain(&peer, 1, filled - 1, &mut pbuf);
            // The blocked write must now be received too (exactly once).
            drain(&peer, filled, 1, &mut pbuf);
            writer
                .join()
                .unwrap()
                .expect("blocked send_raw_blocking failed");
            let woke_after = Duration::from_micros(done_at.load(Ordering::SeqCst))
                .saturating_sub(drain_started_at);
            eprintln!(
                "{end:?} iteration {iteration}: filled {filled} messages; blocked writer \
                 completed {woke_after:?} after the peer started draining"
            );
            assert!(
                woke_after < PROMPT,
                "blocked writer woke late: {woke_after:?}"
            );

            assert_kind(
                peer.try_recv_raw(&mut pbuf),
                io::ErrorKind::WouldBlock,
                "no extra (duplicated) messages",
            );
            assert_nowait(&me, "after backpressure round");
        }
    });
}

#[test]
fn backpressure_without_silent_drops() {
    check_backpressure_without_silent_drops(End::Client);
}

#[test]
fn server_backpressure_without_silent_drops() {
    check_backpressure_without_silent_drops(End::Server);
}

/// One thread blocked in `recv_raw_blocking` and another in `send_raw_blocking` on the same
/// end share one auto-reset readiness event; each must still be woken by its own event.
fn check_concurrent_blocked_reader_and_writer_both_wake(end: End) {
    with_timeout(Duration::from_secs(120), move || {
        let (me, peer) = pair(end);
        let me = Arc::new(me);
        let mut pbuf = recv_buf();
        for round in 0..6u32 {
            let reply_first = round % 2 == 0;
            let filled = fill(&me);

            let reader = {
                let me = me.clone();
                thread::spawn(move || {
                    let mut buf = recv_buf();
                    let (n, _) = me.recv_raw_blocking(&mut buf)?;
                    Ok::<_, io::Error>((n, seq_of(&buf), Instant::now()))
                })
            };
            let writer = {
                let me = me.clone();
                thread::spawn(move || {
                    me.send_raw_blocking(&mut msg(filled, FILL_PAYLOAD), &[])?;
                    Ok::<_, io::Error>(Instant::now())
                })
            };
            thread::sleep(Duration::from_millis(200));
            assert!(!reader.is_finished() && !writer.is_finished());

            let reply = || {
                let at = Instant::now();
                peer.send_raw_blocking(&mut msg(1000 + round, 8), &[])
                    .unwrap();
                at
            };
            if reply_first {
                let sent_at = reply();
                let (n, seq, got_at) = reader.join().unwrap().unwrap();
                assert_eq!((n, seq), (8, 1000 + round));
                assert!(got_at - sent_at < PROMPT, "reader woke late");
                thread::sleep(Duration::from_millis(100));
                assert!(!writer.is_finished(), "writer finished with a full pipe");
                let drained_at = Instant::now();
                drain(&peer, 0, filled + 1, &mut pbuf);
                let wrote_at = writer.join().unwrap().unwrap();
                assert!(wrote_at - drained_at < Duration::from_secs(2));
            } else {
                drain(&peer, 0, 1, &mut pbuf);
                let drained_at = Instant::now();
                drain(&peer, 1, filled, &mut pbuf);
                let wrote_at = writer.join().unwrap().unwrap();
                assert!(wrote_at - drained_at < PROMPT, "writer woke late");
                assert!(!reader.is_finished(), "reader finished without a message");
                let sent_at = reply();
                let (n, seq, got_at) = reader.join().unwrap().unwrap();
                assert_eq!((n, seq), (8, 1000 + round));
                assert!(got_at - sent_at < PROMPT, "reader woke late");
            }
            assert_kind(
                peer.try_recv_raw(&mut pbuf),
                io::ErrorKind::WouldBlock,
                "no extra messages",
            );
        }
        assert_nowait(&me, "after concurrent waits");
    });
}

#[test]
fn concurrent_blocked_reader_and_writer_both_wake() {
    check_concurrent_blocked_reader_and_writer_both_wake(End::Client);
}

#[test]
fn server_concurrent_blocked_reader_and_writer_both_wake() {
    check_concurrent_blocked_reader_and_writer_both_wake(End::Server);
}

/// Peer close while blocked in `recv_raw_blocking` -> prompt error, not a hang.
fn check_peer_close_wakes_blocked_recv(end: End) {
    with_timeout(Duration::from_secs(30), move || {
        let (me, peer) = pair(end);
        let reader = thread::spawn(move || {
            let mut buf = recv_buf();
            let r = me.recv_raw_blocking(&mut buf).map(|(n, _)| n);
            (r, Instant::now(), me)
        });
        thread::sleep(Duration::from_millis(200));
        let closed_at = Instant::now();
        drop(peer);
        let (r, returned_at, me) = reader.join().unwrap();
        assert_kind(r, io::ErrorKind::BrokenPipe, "recv after peer close");
        assert!(
            returned_at - closed_at < PROMPT,
            "recv noticed the close late"
        );

        let mut buf = recv_buf();
        assert_kind(
            me.try_recv_raw(&mut buf),
            io::ErrorKind::BrokenPipe,
            "try_recv_raw after peer close",
        );
        assert_kind(
            me.try_send_raw(&mut msg(0, 8), &[]),
            io::ErrorKind::BrokenPipe,
            "try_send_raw after peer close",
        );
        assert_kind(
            me.send_raw_blocking(&mut msg(0, 8), &[]),
            io::ErrorKind::BrokenPipe,
            "send_raw_blocking after peer close",
        );
        assert_nowait(&me, "after peer close");
    });
}

#[test]
fn peer_close_wakes_blocked_recv() {
    check_peer_close_wakes_blocked_recv(End::Client);
}

#[test]
fn server_peer_close_wakes_blocked_recv() {
    check_peer_close_wakes_blocked_recv(End::Server);
}

/// Peer close while blocked in `send_raw_blocking` on a full pipe -> prompt error.
fn check_peer_close_wakes_blocked_send(end: End) {
    with_timeout(Duration::from_secs(60), move || {
        let (me, peer) = pair(end);
        let filled = fill(&me);
        assert!(filled > 0);
        let writer = thread::spawn(move || {
            let r = me.send_raw_blocking(&mut msg(filled, FILL_PAYLOAD), &[]);
            (r, Instant::now(), me)
        });
        thread::sleep(Duration::from_millis(200));
        assert!(!writer.is_finished());
        let closed_at = Instant::now();
        drop(peer);
        let (r, returned_at, me) = writer.join().unwrap();
        assert_kind(
            r,
            io::ErrorKind::BrokenPipe,
            "blocked send after peer close",
        );
        assert!(
            returned_at - closed_at < PROMPT,
            "send noticed the close late"
        );
        assert_kind(
            me.try_send_raw(&mut msg(0, 8), &[]),
            io::ErrorKind::BrokenPipe,
            "try_send_raw after peer close",
        );
        assert_nowait(&me, "after peer close");
    });
}

#[test]
fn peer_close_wakes_blocked_send() {
    check_peer_close_wakes_blocked_send(End::Client);
}

#[test]
fn server_peer_close_wakes_blocked_send() {
    check_peer_close_wakes_blocked_send(End::Server);
}

/// Messages the peer wrote before closing are still delivered, then the close is reported.
fn check_queued_messages_delivered_before_peer_close_error(end: End) {
    with_timeout(Duration::from_secs(30), move || {
        let (me, peer) = pair(end);
        peer.send_raw_blocking(&mut msg(1, 8), &[]).unwrap();
        peer.send_raw_blocking(&mut msg(2, 8), &[]).unwrap();
        drop(peer);
        let mut buf = recv_buf();
        let mut got = Vec::new();
        let err = loop {
            match me.recv_raw_blocking(&mut buf) {
                Ok((n, _)) => got.push(seq_of(&buf[..n])),
                Err(e) => break e,
            }
        };
        eprintln!("{end:?}: received {got:?} before {err:?}");
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(got, [1, 2]);
    });
}

#[test]
fn queued_messages_delivered_before_peer_close_error() {
    check_queued_messages_delivered_before_peer_close_error(End::Client);
}

#[test]
fn server_queued_messages_delivered_before_peer_close_error() {
    check_queued_messages_delivered_before_peer_close_error(End::Server);
}

/// A message larger than the pipe quota fails explicitly instead of blocking forever; a message
/// of exactly the quota still goes through.
fn check_oversized_message_is_rejected(end: End) {
    with_timeout(Duration::from_secs(60), move || {
        let (mut me, peer) = pair(end);
        // Wire size = payload + 4-byte handle count; the quota is max_message_size() +
        // HANDLE_SUFFIX_SIZE.
        let quota = max_message_size() + HANDLE_SUFFIX_SIZE;
        let mut too_big = vec![1u8; quota - 4 + 1];
        assert_kind(
            me.try_send_raw(&mut too_big, &[]),
            io::ErrorKind::InvalidInput,
            "try_send_raw of an oversized message",
        );
        assert_kind(
            me.send_raw_blocking(&mut too_big, &[]),
            io::ErrorKind::InvalidInput,
            "send_raw_blocking of an oversized message",
        );
        assert_eq!(too_big.len(), quota - 4 + 1, "data must be left untouched");
        assert_nowait(&me, "after oversized send");

        me.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut exact = msg(42, quota - 4);
        me.send_raw_blocking(&mut exact, &[])
            .expect("a message of exactly the pipe quota must be writable");
        let mut pbuf = recv_buf();
        let (n, _) = peer.recv_raw_blocking(&mut pbuf).unwrap();
        assert_eq!((n, seq_of(&pbuf)), (quota - 4, 42));
        assert_nowait(&me, "after quota-sized send");
    });
}

#[test]
fn oversized_message_is_rejected() {
    check_oversized_message_is_rejected(End::Client);
}

#[test]
fn server_oversized_message_is_rejected() {
    check_oversized_message_is_rejected(End::Server);
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Number of instances of the listener's pipe name currently open (in any process).
fn pipe_instances(listener: &SeqpacketListener) -> u32 {
    pipe_handle_state(listener.as_raw_handle() as HANDLE).1
}

/// Poll until the listener's pipe has exactly `expected` instances.
#[track_caller]
fn wait_for_instances(listener: &SeqpacketListener, expected: u32, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let n = pipe_instances(listener);
        if n == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: {n} pipe instances, expected {expected}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

/// Accept one client with `accept_async` and check the connection end to end.
fn accept_async_round_trip(rt: &tokio::runtime::Runtime, listener: &SeqpacketListener, path: &str) {
    let connect_path = path.to_owned();
    let client = thread::spawn(move || SeqpacketConn::connect(connect_path));
    let server = rt.block_on(listener.accept_async()).expect("accept_async");
    assert_nowait(&server, "server end right after accept_async");
    let client = client.join().unwrap().expect("connect");
    assert_eq!(client.peer_credentials().unwrap().pid, std::process::id());

    let (mut sbuf, mut cbuf) = (recv_buf(), recv_buf());
    client.send_raw_blocking(&mut msg(11, 32), &[]).unwrap();
    let (n, _) = server.recv_raw_blocking(&mut sbuf).unwrap();
    assert_eq!((n, seq_of(&sbuf)), (32, 11));
    server.send_raw_blocking(&mut msg(12, 32), &[]).unwrap();
    let (n, _) = client.recv_raw_blocking(&mut cbuf).unwrap();
    assert_eq!((n, seq_of(&cbuf)), (32, 12));
    assert_nowait(&server, "accept_async server end after round trip");
}

/// Regression test for the cancellable accept: dropping an `accept_async` future before a client
/// connects (as the sidecar's `select!` shutdown does) must make its worker thread cancel the
/// overlapped `ConnectNamedPipe`, close the pipe instance it was accepting on and exit, and the
/// listener must keep accepting cleanly (with `accept_async` and `try_accept`) afterwards.
///
/// The worker thread owns the instance it accepts on and closes it as its very last action, so
/// the listener's pipe instance count dropping back to 1 (the listener's own spare instance)
/// shows that the worker finished.  A leaked worker would also leave that instance listening,
/// and a later client could connect to it and then hang forever waiting for the PID handshake.
#[test]
fn accept_async_cancellation_releases_worker_and_listener_keeps_working() {
    with_timeout(Duration::from_secs(120), || {
        let path = unique_pipe_path(line!());
        let listener = SeqpacketListener::bind(&path).unwrap();
        let rt = runtime();
        assert_eq!(pipe_instances(&listener), 1);

        for round in 0..25 {
            rt.block_on(async {
                tokio::select! {
                    // Poll the accept first, so that it starts its worker thread and parks in
                    // the overlapped ConnectNamedPipe; then the ready branch wins and the accept
                    // future is dropped.
                    biased;
                    r = listener.accept_async() => {
                        panic!("accept_async completed without a client: {:?}", r.map(|_| ()))
                    }
                    _ = std::future::ready(()) => {}
                }
            });
            wait_for_instances(
                &listener,
                1,
                &format!("round {round}: after dropping accept_async"),
            );
        }

        // The listener still works, repeatedly, with both accept flavours.
        for _ in 0..3 {
            accept_async_round_trip(&rt, &listener, &path);
            wait_for_instances(&listener, 1, "after accept_async connection closed");
        }
        let connect_path = path.clone();
        let client = thread::spawn(move || SeqpacketConn::connect(connect_path));
        let server = loop {
            match listener.try_accept() {
                Ok(c) => break c,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1))
                }
                Err(e) => panic!("try_accept after cancelled accepts: {e:?}"),
            }
        };
        assert_nowait(&server, "server end right after try_accept");
        let client = client.join().unwrap().expect("connect");
        let mut sbuf = recv_buf();
        client.send_raw_blocking(&mut msg(13, 8), &[]).unwrap();
        let (n, _) = server.recv_raw_blocking(&mut sbuf).unwrap();
        assert_eq!((n, seq_of(&sbuf)), (8, 13));

        // Cancelling again after successful accepts works too.
        drop((server, client));
        rt.block_on(async {
            tokio::select! {
                biased;
                r = listener.accept_async() => {
                    panic!("accept_async completed without a client: {:?}", r.map(|_| ()))
                }
                _ = std::future::ready(()) => {}
            }
        });
        wait_for_instances(&listener, 1, "after the final cancelled accept_async");
        accept_async_round_trip(&rt, &listener, &path);
    });
}

/// The server end handed out by `accept_async` behaves like any other PIPE_NOWAIT end under
/// backpressure: a full pipe is a real `WouldBlock`, never a silently dropped message.
#[test]
fn accept_async_server_end_backpressure() {
    with_timeout(Duration::from_secs(60), || {
        let path = unique_pipe_path(line!());
        let listener = SeqpacketListener::bind(&path).unwrap();
        let rt = runtime();
        let connect_path = path.clone();
        let client = thread::spawn(move || SeqpacketConn::connect(connect_path));
        let server = rt.block_on(listener.accept_async()).expect("accept_async");
        let client = client.join().unwrap().expect("connect");

        let filled = fill(&server);
        assert!(filled > 0);
        assert!(
            filled as usize * (FILL_PAYLOAD + 4) <= max_message_size() + HANDLE_SUFFIX_SIZE,
            "server reported more sends than fit into the pipe"
        );
        let mut cbuf = recv_buf();
        drain(&client, 0, filled, &mut cbuf);
        assert_kind(
            client.try_recv_raw(&mut cbuf),
            io::ErrorKind::WouldBlock,
            "no extra messages",
        );
        assert_nowait(&server, "accept_async server end after backpressure");
    });
}
