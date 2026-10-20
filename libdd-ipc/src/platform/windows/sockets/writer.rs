// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{PendingHandleTransfers, append_handle_suffix, make_overlapped};
use libdd_common::MutexExt;
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::io;
use std::os::windows::io::{OwnedHandle, RawHandle};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Condvar, Mutex};
use windows_sys::Win32::Foundation::{
    ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, ERROR_WRITE_FAULT, GetLastError, HANDLE,
};
use windows_sys::Win32::Storage::FileSystem::{SetFileCompletionNotificationModes, WriteFile};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{
    CancelThreadpoolIo, CloseThreadpoolIo, CreateThreadpoolIo, PTP_CALLBACK_INSTANCE, PTP_IO,
    StartThreadpoolIo, WaitForThreadpoolIoCallbacks,
};
use windows_sys::Win32::System::WindowsProgramming::FILE_SKIP_COMPLETION_PORT_ON_SUCCESS;

/// Serializes writes through a single owned in-flight slot.
///
/// Each admitted write keeps its buffer, `OVERLAPPED` state, and handle-transfer
/// bookkeeping alive until Windows completes it. Immediate completions retire
/// inline; pending completions retire in a Windows thread-pool I/O callback.
/// While the slot is occupied, nonblocking sends return `WouldBlock` and
/// blocking sends wait. Asynchronous errors are returned to a waiting caller
/// and saved for later calls.
///
/// This strategy was chosen because Unix nonblocking behavior cannot safely be
/// simulated by canceling a `WriteFile` that returns `ERROR_IO_PENDING` and
/// translating a final `ERROR_OPERATION_ABORTED` into `WouldBlock`, like is
/// done for reads in `PipeReader`. The Named Pipe File System (NPFS) can queue
/// the whole buffer while its I/O Request Packet (IRP) remains pending for
/// quota. The peer may consume those bytes before NPFS atomically claims the
/// write IRP's completion. If cancellation claimed it first, NPFS cannot undo
/// the peer read and reports zero bytes despite delivery, so a retry could send
/// the same message twice.
pub(super) struct PipeWriter {
    pipe: HANDLE,
    io: PTP_IO,
    // The Box never moves its allocation. Windows borrows this context until Drop
    // has canceled I/O and waited for all callbacks.
    context: Box<WriteContext>,
}

impl PipeWriter {
    pub(super) fn new(pipe: HANDLE) -> Result<Self, i32> {
        let context = Box::new(WriteContext {
            state: Mutex::new(WriteState::default()),
            available: Condvar::new(),
        });
        let io = unsafe {
            CreateThreadpoolIo(
                pipe,
                Some(write_complete),
                (&*context as *const WriteContext).cast_mut().cast(),
                null(),
            )
        };
        if io == 0 {
            return Err(unsafe { GetLastError() } as i32);
        }
        // Immediate successes must release the slot inline, allowing a caller to
        // drain several messages without waiting for thread-pool scheduling.
        if unsafe {
            SetFileCompletionNotificationModes(pipe, FILE_SKIP_COMPLETION_PORT_ON_SUCCESS as u8)
        } == 0
        {
            let error = unsafe { GetLastError() } as i32;
            unsafe { CloseThreadpoolIo(io) };
            return Err(error);
        }
        Ok(Self { pipe, io, context })
    }

    pub(super) fn send(
        &self,
        data: Vec<u8>,
        handles: &[RawHandle],
        peer_pid: u32,
        blocking: bool,
    ) -> io::Result<()> {
        // This guard reserves the sole slot before copying or duplicating handles.
        // It also prevents the callback from retiring storage before WriteFile's
        // return and its notification accounting have been inspected.
        let mut state = self.context.state.lock_or_panic();
        loop {
            state.check_error()?;
            if state.pending.is_none() {
                break;
            }
            if !blocking {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            state = self
                .context
                .available
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner());
        }

        let mut wire = data;
        let transfers = append_handle_suffix(&mut wire, handles, peer_pid, &mut state.peer)?;
        let len = u32::try_from(wire.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "IPC message is too large"))?;
        let completion = Arc::new(Completion::default());
        let pending = Box::new(PendingWrite {
            overlapped: UnsafeCell::new(make_overlapped(0)),
            wire,
            transfers,
            completion: Arc::clone(&completion),
        });
        let overlapped = pending.overlapped.get();
        let bytes = pending.wire.as_ptr();
        state.pending = Some(pending);
        unsafe { StartThreadpoolIo(self.io) };
        let submitted = unsafe { WriteFile(self.pipe, bytes, len, null_mut(), overlapped) };
        let error = if submitted == 0 {
            unsafe { GetLastError() }
        } else {
            0
        };
        if submitted != 0 || error == ERROR_IO_PENDING {
            // Once accepted, even a failed/canceled write may have exposed remote
            // handle values. An immediate submission failure cannot have done so.
            #[allow(clippy::expect_used)]
            let pending = state
                .pending
                .as_mut()
                .expect("pending write is protected by the state lock");
            pending.transfers.expose();
        }
        if error != ERROR_IO_PENDING {
            // immediate success or error
            // balance out StartThreadpoolIo above
            // This is needed even with FILE_SKIP_COMPLETION_PORT_ON_SUCCESS
            unsafe { CancelThreadpoolIo(self.io) };
            let mut written = 0;
            let result = if error != 0 {
                Err(error)
            } else if unsafe { GetOverlappedResult(self.pipe, overlapped, &mut written, 0) } == 0 {
                Err(unsafe { GetLastError() })
            } else if written != len {
                Err(ERROR_WRITE_FAULT)
            } else {
                Ok(())
            };
            self.context.finish(&mut state, result);
            return result.map_err(os_error);
        }
        drop(state); // unlock before waiting for completion (callback takes same lock)
        if blocking {
            completion.wait().map_err(os_error)
        } else {
            // Accepted: a later failure is latched, never converted to WouldBlock.
            Ok(())
        }
    }

    pub(super) fn check_error(&self) -> io::Result<()> {
        self.context.state.lock_or_panic().check_error()
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        {
            let mut state = self.context.state.lock_or_panic();
            state.closing = true;
            if let Some(pending) = &state.pending {
                // Real shutdown cancellation, never an admission/capacity probe.
                // The callback retires the buffer even if CancelIoEx loses the race.
                unsafe { CancelIoEx(self.pipe, pending.overlapped.get()) };
            }
            // wait for callback to retire the slot via finish()
            // The loop is only needed for spurious awakenings, because closing = true
            // ensures there's no new transition to pending = Some
            #[allow(clippy::expect_used)]
            let state = self
                .context
                .available
                .wait_while(state, |state| state.pending.is_some())
                .expect("WriteContext::available is poisoned");
            drop(state);
        }
        // No lock needed by callbacks is held here. A callback may have retired
        // its slot but still be returning; wait before freeing its context/code.
        unsafe {
            WaitForThreadpoolIoCallbacks(self.io, 0);
            CloseThreadpoolIo(self.io);
        }
    }
}

struct WriteContext {
    state: Mutex<WriteState>,
    // Wakes blocking senders and teardown when the pending-write slot is released.
    available: Condvar,
}

impl WriteContext {
    fn finish(&self, state: &mut WriteState, result: Result<(), u32>) {
        if let Some(pending) = state.pending.take() {
            if let Err(error) = result {
                state.error = Some(error);
            }
            *pending.completion.result.lock_or_panic() = Some(result);
            pending.completion.ready.notify_all();
            // Terminal I/O no longer accesses the Vec or OVERLAPPED. The exposed
            // transfer guard only releases its local process handle, never remote IDs.
            drop(pending);
            self.available.notify_all();
        }
    }
}

#[derive(Default)]
struct WriteState {
    pending: Option<Box<PendingWrite>>,
    peer: Option<Arc<OwnedHandle>>,
    error: Option<u32>,
    closing: bool,
}

impl WriteState {
    fn check_error(&self) -> io::Result<()> {
        if let Some(error) = self.error {
            Err(os_error(error))
        } else if self.closing {
            Err(os_error(ERROR_OPERATION_ABORTED))
        } else {
            Ok(())
        }
    }
}

struct PendingWrite {
    // Only Windows mutates this while pending; Rust accesses it through the raw
    // pointer. Box allocation and Vec contents are stable until terminal completion.
    overlapped: UnsafeCell<OVERLAPPED>,
    wire: Vec<u8>,
    transfers: PendingHandleTransfers,
    completion: Arc<Completion>,
}

// Access is serialized by WriteContext::state. Moving the Box between threads
// does not move Windows' operation storage. Remote HANDLE values are opaque IDs;
// their exposed guard never closes them, and OwnedHandle owns the process handle.
unsafe impl Send for PendingWrite {}

#[derive(Default)]
struct Completion {
    result: Mutex<Option<Result<(), u32>>>,
    ready: Condvar,
}

impl Completion {
    fn wait(&self) -> Result<(), u32> {
        let mut result = self.result.lock_or_panic();
        loop {
            if let Some(result) = *result {
                return result;
            }
            result = self
                .ready
                .wait(result)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }
}

unsafe extern "system" fn write_complete(
    _instance: PTP_CALLBACK_INSTANCE,
    context: *mut c_void,
    overlapped: *mut c_void,
    result: u32,
    transferred: usize,
    _io: PTP_IO,
) {
    // Context is borrowed, never reference-counted/destroyed by the callback.
    // PipeWriter::drop waits for terminal I/O and callback quiescence. Only this
    // writer issues notification-bearing I/O; reads use low-bit-tagged events.
    // This path performs no user calls or allocations. A poisoned lock panics.
    let context = unsafe { &*context.cast::<WriteContext>() };
    let mut state = context.state.lock_or_panic();
    let Some(pending) = &state.pending else {
        return;
    };
    if pending.overlapped.get().cast::<c_void>() != overlapped {
        return;
    }
    let result = if result != 0 {
        Err(result)
    } else if transferred != pending.wire.len() {
        Err(ERROR_WRITE_FAULT)
    } else {
        Ok(())
    };
    context.finish(&mut state, result);
}

fn os_error(error: u32) -> io::Error {
    // Win32 error codes are represented as DWORDs by the API and i32 by std::io.
    io::Error::from_raw_os_error(error as i32)
}

#[cfg(test)]
mod tests {
    use libdd_common::MutexExt;

    use super::*;
    use crate::platform::windows::sockets::{HANDLE_SUFFIX_SIZE, SeqpacketConn, max_message_size};
    use std::mem::size_of;
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);
    const BLOCK_CHECK_TIMEOUT: Duration = Duration::from_millis(50);

    #[test]
    fn nine_synchronous_writes_retire_inline() {
        let (sender, receiver) = SeqpacketConn::socketpair().unwrap();
        for id in 0..9 {
            sender.try_send_raw(vec![id], &[]).unwrap();
            assert!(
                sender
                    .writer()
                    .unwrap()
                    .context
                    .state
                    .lock_or_panic()
                    .pending
                    .is_none()
            );
        }
        let mut buffer = [0; 1 + HANDLE_SUFFIX_SIZE];
        for id in 0..9 {
            assert_eq!(receiver.recv_raw_blocking(&mut buffer).unwrap().0, 1);
            assert_eq!(buffer[0], id);
        }
    }

    #[test]
    fn pending_write_takes_allocation_and_rejects_before_handle_preparation() {
        let (sender, receiver, count) = pair_with_pending_write();
        // An invalid handle would fail DuplicateHandle if preparation were reached.
        let error = sender.try_send_raw(vec![17], &[null_mut()]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        receive_unsequenced_fill(&receiver, count);
        wait_for_write_retirement(&sender);
        sender.try_send_raw(vec![17], &[]).unwrap();
        let mut buffer = [0; 1 + HANDLE_SUFFIX_SIZE];
        assert_eq!(receiver.recv_raw_blocking(&mut buffer).unwrap().0, 1);
        assert_eq!(buffer[0], 17);
        assert_eq!(
            receiver.try_recv_raw(&mut buffer).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn repeated_backpressure_preserves_messages_and_blocking_successor_order() {
        let (sender, receiver) = SeqpacketConn::socketpair().unwrap();
        let sender = Arc::new(sender);
        let mut buffer = vec![0; max_message_size() + HANDLE_SUFFIX_SIZE];
        let mut next = 0;

        for _ in 0..3 {
            let accepted = fill_until_backpressured(&sender, next);

            let successor = next + accepted;
            let (writer, done_rx) = start_blocking_send(&sender, large_message(successor));
            assert_still_blocked(&done_rx, "blocking send on a full pipe");

            // Draining retires the pending write and admits the blocking successor.
            // Every accepted message must arrive exactly once, in submission order.
            for sequence in next..successor {
                receive_large(&receiver, sequence, &mut buffer);
            }
            receive_large(&receiver, successor, &mut buffer);
            done_rx.recv_timeout(TEST_TIMEOUT).unwrap().unwrap();
            writer.join().unwrap();

            assert_eq!(
                receiver.try_recv_raw(&mut buffer).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            next = successor + 1;
        }
    }

    #[test]
    fn peer_close_wakes_sender_waiting_for_pending_slot() {
        let (sender, receiver, _) = pair_with_pending_write();
        let sender = Arc::new(sender);
        let (writer, done_rx) = start_blocking_send(&sender, vec![1]);
        assert_still_blocked(&done_rx, "sender waiting for the pending-write slot");

        // The accepted write may retire successfully after peer closure. The
        // waiting sender must still wake and detect the closure on its write.
        drop(receiver);
        let error = done_rx
            .recv_timeout(TEST_TIMEOUT)
            .expect("blocked sender did not wake after peer closure")
            .unwrap_err();
        assert_ne!(error.kind(), io::ErrorKind::WouldBlock);
        writer.join().unwrap();
        assert!(sender.check_write_error().is_err());
        assert!(sender.drain_acks_nonblocking(0).is_err());
    }

    #[test]
    fn canceled_completion_latches_error_instead_of_refusing() {
        let (sender, receiver, _) = pair_with_pending_write();
        let writer = sender.writer().unwrap();
        {
            let state = writer.context.state.lock_or_panic();
            let pending = state.pending.as_ref().unwrap();
            assert_ne!(
                unsafe { CancelIoEx(writer.pipe, pending.overlapped.get()) },
                0
            );
        }
        wait_for_write_retirement(&sender);
        let error = sender.try_send_raw(vec![1], &[]).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(ERROR_OPERATION_ABORTED as i32));
        assert_ne!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(sender.drain_acks_nonblocking(0).is_err());
        drop(receiver);
    }

    #[test]
    fn drop_cancels_and_reaps_pending_write() {
        let (sender, receiver, _) = pair_with_pending_write();
        let (tx, rx) = mpsc::sync_channel(1);
        let writer = std::thread::spawn(move || {
            drop(sender);
            tx.send(()).unwrap();
        });
        rx.recv_timeout(TEST_TIMEOUT)
            .expect("Drop did not quiesce pending I/O");
        writer.join().unwrap();
        drop(receiver);
    }

    #[test]
    fn concurrent_read_and_backpressured_write_complete_in_either_order() {
        use ReleaseOrder::{DrainThenReply, ReplyThenDrain};

        enum ReleaseOrder {
            ReplyThenDrain,
            DrainThenReply,
        }

        let (server, client) = SeqpacketConn::socketpair().unwrap();
        // Event reads must not produce unreserved callbacks after either handle
        // has been registered for thread-pool I/O by its writer.
        server.writer().unwrap();
        client.writer().unwrap();
        let client = Arc::new(client);
        let mut server_buffer = vec![0; max_message_size() + HANDLE_SUFFIX_SIZE];
        let mut next_sequence = 0;

        // Repeat both orders on the same connection to exercise slot reuse.
        let release_orders = [
            ReplyThenDrain,
            DrainThenReply,
            ReplyThenDrain,
            DrainThenReply,
        ];
        for (round, release_order) in release_orders.into_iter().enumerate() {
            let first = next_sequence;
            let accepted = fill_until_backpressured(&client, first);
            let successor = first + accepted;
            let reply = round as u32;

            // The client has a full outbound pipe and no inbound data. Its read
            // and blocking successor therefore need independent peer actions.
            let (reader_started_tx, reader_started_rx) = mpsc::sync_channel(1);
            let (reader_done_tx, reader_done_rx) = mpsc::sync_channel(1);
            let reader_client = Arc::clone(&client);
            let reader = std::thread::spawn(move || {
                reader_started_tx.send(()).unwrap();
                let mut buffer = vec![0; max_message_size() + HANDLE_SUFFIX_SIZE];
                let result = reader_client
                    .recv_raw_blocking(&mut buffer)
                    .map(|(len, _)| (len, message_sequence(&buffer[..len])));
                reader_done_tx.send(result).unwrap();
            });

            let (writer, writer_done_rx) = start_blocking_send(&client, large_message(successor));

            reader_started_rx.recv_timeout(TEST_TIMEOUT).unwrap();
            assert_still_blocked(&reader_done_rx, "read with no inbound message");
            assert_still_blocked(&writer_done_rx, "write with a full outbound pipe");

            match release_order {
                ReplyThenDrain => {
                    // A reply releases the read while the write remains backpressured.
                    server.send_raw_blocking(small_message(reply), &[]).unwrap();
                    assert_eq!(
                        reader_done_rx.recv_timeout(TEST_TIMEOUT).unwrap().unwrap(),
                        (size_of::<u32>(), reply)
                    );
                    assert_still_blocked(&writer_done_rx, "write before the peer drains");

                    for sequence in first..=successor {
                        receive_large(&server, sequence, &mut server_buffer);
                    }
                    writer_done_rx.recv_timeout(TEST_TIMEOUT).unwrap().unwrap();
                }
                DrainThenReply => {
                    // Draining releases the write while the read still lacks a reply.
                    for sequence in first..=successor {
                        receive_large(&server, sequence, &mut server_buffer);
                    }
                    writer_done_rx.recv_timeout(TEST_TIMEOUT).unwrap().unwrap();
                    assert_still_blocked(&reader_done_rx, "read before the peer replies");

                    server.send_raw_blocking(small_message(reply), &[]).unwrap();
                    assert_eq!(
                        reader_done_rx.recv_timeout(TEST_TIMEOUT).unwrap().unwrap(),
                        (size_of::<u32>(), reply)
                    );
                }
            }

            reader.join().unwrap();
            writer.join().unwrap();
            assert_eq!(
                server.try_recv_raw(&mut server_buffer).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            next_sequence = successor + 1;
        }
    }

    #[test]
    fn peer_close_wakes_blocked_recv() {
        let (server, client) = SeqpacketConn::socketpair().unwrap();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let mut buffer = vec![0; max_message_size() + HANDLE_SUFFIX_SIZE];
            let result = client.recv_raw_blocking(&mut buffer).map(|(len, _)| len);
            done_tx.send((result, client)).unwrap();
        });
        started_rx.recv_timeout(TEST_TIMEOUT).unwrap();
        assert_still_blocked(&done_rx, "read with no inbound message");

        drop(server);
        let (result, client) = done_rx
            .recv_timeout(TEST_TIMEOUT)
            .expect("blocked read did not wake after peer closure");
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        reader.join().unwrap();

        let mut buffer = vec![0; max_message_size() + HANDLE_SUFFIX_SIZE];
        assert_eq!(
            client.try_recv_raw(&mut buffer).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_ne!(
            client
                .try_send_raw(small_message(0), &[])
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }

    fn pair_with_pending_write() -> (SeqpacketConn, SeqpacketConn, usize) {
        let (sender, receiver) = SeqpacketConn::socketpair().unwrap();
        for count in 1..=16 {
            let mut payload = Vec::with_capacity(max_message_size() + HANDLE_SUFFIX_SIZE);
            payload.resize(max_message_size(), 0x5a);
            let allocation = payload.as_ptr();
            sender.try_send_raw(payload, &[]).unwrap();
            let state = sender.writer().unwrap().context.state.lock_or_panic();
            if let Some(pending) = &state.pending {
                assert_eq!(
                    pending.wire.as_ptr(),
                    allocation,
                    "the pending write must retain the caller's allocation"
                );
                drop(state);
                return (sender, receiver, count);
            }
        }
        panic!("pipe did not produce a pending write");
    }

    fn wait_for_write_retirement(sender: &SeqpacketConn) {
        let writer = sender.writer().unwrap();
        let deadline = Instant::now() + TEST_TIMEOUT;
        let mut state = writer.context.state.lock_or_panic();
        while state.pending.is_some() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "pending write did not retire");
            state = writer
                .context
                .available
                .wait_timeout(state, remaining)
                .unwrap()
                .0;
        }
    }

    fn receive_unsequenced_fill(receiver: &SeqpacketConn, count: usize) {
        let mut buffer = vec![0; max_message_size() + HANDLE_SUFFIX_SIZE];
        for _ in 0..count {
            let (n, handles) = receiver.recv_raw_blocking(&mut buffer).unwrap();
            assert_eq!(n, max_message_size());
            assert!(buffer[..n].iter().all(|byte| *byte == 0x5a));
            assert!(handles.is_empty());
        }
    }

    fn sequenced_message(sequence: u32, len: usize) -> Vec<u8> {
        assert!(len >= size_of::<u32>());
        let mut message = Vec::with_capacity(len + HANDLE_SUFFIX_SIZE);
        message.extend_from_slice(&sequence.to_le_bytes());
        message.resize(len, 0x5a);
        message
    }

    fn large_message(sequence: u32) -> Vec<u8> {
        sequenced_message(sequence, max_message_size())
    }

    fn small_message(sequence: u32) -> Vec<u8> {
        sequenced_message(sequence, size_of::<u32>())
    }

    fn message_sequence(message: &[u8]) -> u32 {
        u32::from_le_bytes(message[..size_of::<u32>()].try_into().unwrap())
    }

    /// Returns the number of accepted messages, including the pending write.
    fn fill_until_backpressured(sender: &Arc<SeqpacketConn>, first: u32) -> u32 {
        // Bound the whole fill attempt: a regression that blocks inside try_send_raw
        // must fail the test even though the peer deliberately does not read yet.
        let sender = Arc::clone(sender);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            for count in 0..16u32 {
                match sender.try_send_raw(large_message(first + count), &[]) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(count > 0, "the empty pipe rejected the first message");
                        // Rejections must leave both the pending write and the pipe
                        // contents alone. This sentinel must never reach the peer.
                        for _ in 0..3 {
                            let error = sender
                                .try_send_raw(large_message(u32::MAX), &[])
                                .unwrap_err();
                            assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
                        }
                        done_tx.send(count).unwrap();
                        return;
                    }
                    Err(error) => panic!("failed while filling pipe: {error}"),
                }
            }
            panic!("pipe did not apply backpressure after 16 messages");
        });
        let accepted = done_rx
            .recv_timeout(TEST_TIMEOUT)
            .expect("nonblocking fill did not finish while the peer was idle");
        thread.join().unwrap();
        accepted
    }

    fn receive_large(receiver: &SeqpacketConn, expected: u32, buffer: &mut [u8]) {
        let (len, handles) = receiver.recv_raw_blocking(buffer).unwrap();
        assert_eq!(len, max_message_size(), "truncated message {expected}");
        assert_eq!(
            message_sequence(&buffer[..len]),
            expected,
            "missing, duplicated, or reordered message"
        );
        assert!(
            buffer[size_of::<u32>()..len]
                .iter()
                .all(|byte| *byte == 0x5a),
            "corrupted payload in message {expected}"
        );
        assert!(
            handles.is_empty(),
            "unexpected handles in message {expected}"
        );
    }

    fn start_blocking_send(
        sender: &Arc<SeqpacketConn>,
        message: Vec<u8>,
    ) -> (std::thread::JoinHandle<()>, mpsc::Receiver<io::Result<()>>) {
        let sender = Arc::clone(sender);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx
                .send(sender.send_raw_blocking(message, &[]))
                .unwrap();
        });
        started_rx.recv_timeout(TEST_TIMEOUT).unwrap();
        (thread, done_rx)
    }

    fn assert_still_blocked<T>(receiver: &mpsc::Receiver<T>, operation: &str) {
        // The worker signals just before calling the operation. This observation
        // window gives it time to enter the wait; it is not an OS readiness probe.
        match receiver.recv_timeout(BLOCK_CHECK_TIMEOUT) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("{operation} thread exited without reporting a result")
            }
            Ok(_) => panic!("{operation} completed before its release condition"),
        }
    }
}
