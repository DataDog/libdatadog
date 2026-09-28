// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(windows)]

//! Behaviour of the client end of the Windows named-pipe transport, which is kept in
//! `PIPE_NOWAIT` mode for its whole lifetime and blocks by waiting on the NPFS readiness event.

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
    let mut state: u32 = 0;
    let ok = unsafe {
        GetNamedPipeHandleStateW(
            conn.as_raw_handle() as HANDLE,
            &mut state,
            null_mut(),
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
    state
}

#[track_caller]
fn assert_nowait(conn: &SeqpacketConn, when: &str) {
    let state = pipe_state(conn);
    assert_eq!(
        state & (PIPE_NOWAIT | PIPE_READMODE_MESSAGE),
        PIPE_NOWAIT | PIPE_READMODE_MESSAGE,
        "client pipe state {state:#x} is not PIPE_NOWAIT|PIPE_READMODE_MESSAGE {when}"
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

/// Fill the client -> server direction with `FILL_PAYLOAD`-sized messages until `try_send_raw`
/// reports `WouldBlock`.  Returns the number of messages accepted (sequence numbers `0..n`).
fn fill(client: &SeqpacketConn) -> u32 {
    let mut sent = 0u32;
    loop {
        match client.try_send_raw(&mut msg(sent, FILL_PAYLOAD), &[]) {
            Ok(()) => sent += 1,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) => panic!("try_send_raw failed while filling: {e:?}"),
        }
        assert!(sent < 100_000, "pipe never filled up");
    }
    assert_nowait(client, "after try_send_raw returned WouldBlock");
    sent
}

/// Receive exactly `count` fill messages on the server and check that they arrive in order.
fn drain(server: &SeqpacketConn, first_seq: u32, count: u32, buf: &mut [u8]) {
    for expected in first_seq..first_seq + count {
        let (n, handles) = server.recv_raw_blocking(buf).expect("server recv");
        assert!(handles.is_empty());
        assert_eq!(n, FILL_PAYLOAD, "message {expected} has the wrong length");
        assert_eq!(
            seq_of(buf),
            expected,
            "message lost, duplicated or reordered"
        );
    }
}

/// Requirement: the client handle is PIPE_NOWAIT from connect on, and no operation (success,
/// WouldBlock, timeout) ever switches it back to PIPE_WAIT.
#[test]
fn client_mode_is_nowait_for_entire_lifetime() {
    with_timeout(Duration::from_secs(60), || {
        let (server, mut client) = SeqpacketConn::socketpair().unwrap();
        let mut sbuf = recv_buf();
        let mut cbuf = recv_buf();
        assert_nowait(&client, "after connect");

        assert_kind(
            client.try_recv_raw(&mut cbuf),
            io::ErrorKind::WouldBlock,
            "try_recv_raw on empty pipe",
        );
        assert_nowait(&client, "after try_recv_raw WouldBlock");

        client.try_send_raw(&mut msg(1, 16), &[]).unwrap();
        assert_nowait(&client, "after try_send_raw Ok");
        client.send_raw_blocking(&mut msg(2, 16), &[]).unwrap();
        assert_nowait(&client, "after send_raw_blocking Ok");
        for expected in [1, 2] {
            let (n, _) = server.recv_raw_blocking(&mut sbuf).unwrap();
            assert_eq!((n, seq_of(&sbuf)), (16, expected));
        }

        server.send_raw_blocking(&mut msg(3, 16), &[]).unwrap();
        let (n, _) = client.try_recv_raw(&mut cbuf).unwrap();
        assert_eq!((n, seq_of(&cbuf)), (16, 3));
        assert_nowait(&client, "after try_recv_raw Ok");

        server.send_raw_blocking(&mut msg(4, 16), &[]).unwrap();
        let (n, _) = client.recv_raw_blocking(&mut cbuf).unwrap();
        assert_eq!((n, seq_of(&cbuf)), (16, 4));
        assert_nowait(&client, "after recv_raw_blocking Ok");

        client
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let t = Instant::now();
        assert_kind(
            client.recv_raw_blocking(&mut cbuf),
            io::ErrorKind::TimedOut,
            "recv_raw_blocking with read timeout",
        );
        let waited = t.elapsed();
        assert!(
            waited >= Duration::from_millis(90) && waited < Duration::from_secs(2),
            "read timeout took {waited:?}"
        );
        assert_nowait(&client, "after recv_raw_blocking TimedOut");

        let filled = fill(&client);
        client
            .set_write_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        assert_kind(
            client.send_raw_blocking(&mut msg(filled, FILL_PAYLOAD), &[]),
            io::ErrorKind::TimedOut,
            "send_raw_blocking on a full pipe with write timeout",
        );
        assert_nowait(&client, "after send_raw_blocking TimedOut");

        drain(&server, 0, filled, &mut sbuf);
        assert_kind(
            server.try_recv_raw(&mut sbuf),
            io::ErrorKind::WouldBlock,
            "timed-out send must not have been written",
        );
        assert_nowait(&client, "at end of test");
    });
}

/// Connecting through a real listener: the PID handshake read (which now waits on the
/// readiness event instead of a PIPE_WAIT ReadFile) completes and yields the acceptor's PID.
#[test]
fn listener_connect_handshake_waits_for_pid() {
    with_timeout(Duration::from_secs(60), || {
        let path = format!(
            r"\\.\pipe\libdd-ipc-nowait-test-{}-{}",
            std::process::id(),
            line!()
        );
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
    });
}

/// A blocked receive must wake up as soon as the peer writes, not after a poll interval.
#[test]
fn blocking_recv_wakes_promptly() {
    with_timeout(Duration::from_secs(60), || {
        let (server, client) = SeqpacketConn::socketpair().unwrap();
        let mut cbuf = recv_buf();
        let mut worst = Duration::ZERO;
        for round in 0..10u32 {
            let (tx, rx) = mpsc::channel();
            let writer = thread::scope(|s| {
                s.spawn(|| {
                    thread::sleep(Duration::from_millis(150));
                    let written_at = Instant::now();
                    server.send_raw_blocking(&mut msg(round, 64), &[]).unwrap();
                    tx.send(written_at).unwrap();
                });
                let started = Instant::now();
                let (n, _) = client.recv_raw_blocking(&mut cbuf).unwrap();
                let returned_at = Instant::now();
                assert_eq!((n, seq_of(&cbuf)), (64, round));
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
        eprintln!("worst recv wake-up latency after peer write: {worst:?}");
        assert!(worst < Duration::from_millis(50), "slow wake-up: {worst:?}");
        assert_nowait(&client, "after blocking receives");
    });
}

/// Many blocking round trips; a lost wake-up would hang (and trip the timeout).
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
        assert_nowait(&client, "after ping-pong");
    });
}

/// Backpressure: a full pipe yields a real `WouldBlock` (not a silent zero-byte "success"), a
/// blocking send stays blocked until the peer drains, and every message the client counted as
/// sent is received exactly once, in order.
#[test]
fn backpressure_without_silent_drops() {
    with_timeout(Duration::from_secs(120), || {
        let (server, client) = SeqpacketConn::socketpair().unwrap();
        let client = Arc::new(client);
        let mut sbuf = recv_buf();

        for iteration in 0..3 {
            let filled = fill(&client);
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
                    client.try_send_raw(&mut msg(u32::MAX, FILL_PAYLOAD), &[]),
                    io::ErrorKind::WouldBlock,
                    "try_send_raw on a full pipe",
                );
            }

            let done = Arc::new(AtomicBool::new(false));
            let done_at = Arc::new(AtomicU64::new(0));
            let epoch = Instant::now();
            let writer = {
                let (client, done, done_at) = (client.clone(), done.clone(), done_at.clone());
                thread::spawn(move || {
                    let r = client.send_raw_blocking(&mut msg(filled, FILL_PAYLOAD), &[]);
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
            drain(&server, 0, 1, &mut sbuf);
            drain(&server, 1, filled - 1, &mut sbuf);
            // The blocked write must now be received too (exactly once).
            drain(&server, filled, 1, &mut sbuf);
            writer
                .join()
                .unwrap()
                .expect("blocked send_raw_blocking failed");
            let woke_after = Duration::from_micros(done_at.load(Ordering::SeqCst))
                .saturating_sub(drain_started_at);
            eprintln!(
                "iteration {iteration}: filled {filled} messages; blocked writer completed \
                 {woke_after:?} after the peer started draining"
            );
            assert!(
                woke_after < PROMPT,
                "blocked writer woke late: {woke_after:?}"
            );

            assert_kind(
                server.try_recv_raw(&mut sbuf),
                io::ErrorKind::WouldBlock,
                "no extra (duplicated) messages",
            );
            assert_nowait(&client, "after backpressure round");
        }
    });
}

/// One thread blocked in `recv_raw_blocking` and another in `send_raw_blocking` on the same
/// client share one auto-reset readiness event; each must still be woken by its own event.
#[test]
fn concurrent_blocked_reader_and_writer_both_wake() {
    with_timeout(Duration::from_secs(120), || {
        let (server, client) = SeqpacketConn::socketpair().unwrap();
        let client = Arc::new(client);
        let mut sbuf = recv_buf();
        for round in 0..6u32 {
            let reply_first = round % 2 == 0;
            let filled = fill(&client);

            let reader = {
                let client = client.clone();
                thread::spawn(move || {
                    let mut cbuf = recv_buf();
                    let (n, _) = client.recv_raw_blocking(&mut cbuf)?;
                    Ok::<_, io::Error>((n, seq_of(&cbuf), Instant::now()))
                })
            };
            let writer = {
                let client = client.clone();
                thread::spawn(move || {
                    client.send_raw_blocking(&mut msg(filled, FILL_PAYLOAD), &[])?;
                    Ok::<_, io::Error>(Instant::now())
                })
            };
            thread::sleep(Duration::from_millis(200));
            assert!(!reader.is_finished() && !writer.is_finished());

            let reply = || {
                let at = Instant::now();
                server
                    .send_raw_blocking(&mut msg(1000 + round, 8), &[])
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
                drain(&server, 0, filled + 1, &mut sbuf);
                let wrote_at = writer.join().unwrap().unwrap();
                assert!(wrote_at - drained_at < Duration::from_secs(2));
            } else {
                drain(&server, 0, 1, &mut sbuf);
                let drained_at = Instant::now();
                drain(&server, 1, filled, &mut sbuf);
                let wrote_at = writer.join().unwrap().unwrap();
                assert!(wrote_at - drained_at < PROMPT, "writer woke late");
                assert!(!reader.is_finished(), "reader finished without a message");
                let sent_at = reply();
                let (n, seq, got_at) = reader.join().unwrap().unwrap();
                assert_eq!((n, seq), (8, 1000 + round));
                assert!(got_at - sent_at < PROMPT, "reader woke late");
            }
            assert_kind(
                server.try_recv_raw(&mut sbuf),
                io::ErrorKind::WouldBlock,
                "no extra messages",
            );
        }
        assert_nowait(&client, "after concurrent waits");
    });
}

/// Peer close while blocked in `recv_raw_blocking` -> prompt error, not a hang.
#[test]
fn peer_close_wakes_blocked_recv() {
    with_timeout(Duration::from_secs(30), || {
        let (server, client) = SeqpacketConn::socketpair().unwrap();
        let reader = thread::spawn(move || {
            let mut cbuf = recv_buf();
            let r = client.recv_raw_blocking(&mut cbuf).map(|(n, _)| n);
            (r, Instant::now(), client)
        });
        thread::sleep(Duration::from_millis(200));
        let closed_at = Instant::now();
        drop(server);
        let (r, returned_at, client) = reader.join().unwrap();
        assert_kind(r, io::ErrorKind::BrokenPipe, "recv after peer close");
        assert!(
            returned_at - closed_at < PROMPT,
            "recv noticed the close late"
        );

        let mut cbuf = recv_buf();
        assert_kind(
            client.try_recv_raw(&mut cbuf),
            io::ErrorKind::BrokenPipe,
            "try_recv_raw after peer close",
        );
        assert_kind(
            client.try_send_raw(&mut msg(0, 8), &[]),
            io::ErrorKind::BrokenPipe,
            "try_send_raw after peer close",
        );
        assert_kind(
            client.send_raw_blocking(&mut msg(0, 8), &[]),
            io::ErrorKind::BrokenPipe,
            "send_raw_blocking after peer close",
        );
        assert_nowait(&client, "after peer close");
    });
}

/// Peer close while blocked in `send_raw_blocking` on a full pipe -> prompt error.
#[test]
fn peer_close_wakes_blocked_send() {
    with_timeout(Duration::from_secs(60), || {
        let (server, client) = SeqpacketConn::socketpair().unwrap();
        let filled = fill(&client);
        assert!(filled > 0);
        let writer = thread::spawn(move || {
            let r = client.send_raw_blocking(&mut msg(filled, FILL_PAYLOAD), &[]);
            (r, Instant::now(), client)
        });
        thread::sleep(Duration::from_millis(200));
        assert!(!writer.is_finished());
        let closed_at = Instant::now();
        drop(server);
        let (r, returned_at, client) = writer.join().unwrap();
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
            client.try_send_raw(&mut msg(0, 8), &[]),
            io::ErrorKind::BrokenPipe,
            "try_send_raw after peer close",
        );
        assert_nowait(&client, "after peer close");
    });
}

/// Messages the server wrote before closing are still delivered, then the close is reported.
#[test]
fn queued_messages_delivered_before_peer_close_error() {
    with_timeout(Duration::from_secs(30), || {
        let (server, client) = SeqpacketConn::socketpair().unwrap();
        server.send_raw_blocking(&mut msg(1, 8), &[]).unwrap();
        server.send_raw_blocking(&mut msg(2, 8), &[]).unwrap();
        drop(server);
        let mut cbuf = recv_buf();
        let mut got = Vec::new();
        let err = loop {
            match client.recv_raw_blocking(&mut cbuf) {
                Ok((n, _)) => got.push(seq_of(&cbuf[..n])),
                Err(e) => break e,
            }
        };
        eprintln!("received {got:?} before {err:?}");
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(got, [1, 2]);
    });
}

/// A message larger than the pipe quota fails explicitly instead of blocking forever; a message
/// of exactly the quota still goes through.
#[test]
fn oversized_message_is_rejected() {
    with_timeout(Duration::from_secs(60), || {
        let (server, mut client) = SeqpacketConn::socketpair().unwrap();
        // Wire size = payload + 4-byte handle count; the quota is max_message_size() +
        // HANDLE_SUFFIX_SIZE.
        let quota = max_message_size() + HANDLE_SUFFIX_SIZE;
        let mut too_big = vec![1u8; quota - 4 + 1];
        assert_kind(
            client.try_send_raw(&mut too_big, &[]),
            io::ErrorKind::InvalidInput,
            "try_send_raw of an oversized message",
        );
        assert_kind(
            client.send_raw_blocking(&mut too_big, &[]),
            io::ErrorKind::InvalidInput,
            "send_raw_blocking of an oversized message",
        );
        assert_eq!(too_big.len(), quota - 4 + 1, "data must be left untouched");
        assert_nowait(&client, "after oversized send");

        client
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut exact = msg(42, quota - 4);
        client
            .send_raw_blocking(&mut exact, &[])
            .expect("a message of exactly the pipe quota must be writable");
        let mut sbuf = recv_buf();
        let (n, _) = server.recv_raw_blocking(&mut sbuf).unwrap();
        assert_eq!((n, seq_of(&sbuf)), (quota - 4, 42));
        assert_nowait(&client, "after quota-sized send");
    });
}
