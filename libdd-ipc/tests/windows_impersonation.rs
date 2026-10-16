// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(windows)]

//! Pipes and shared memory must belong to the process identity, which the sidecar runs as, even
//! when created by a thread impersonating a user (here ANONYMOUS LOGON) that may not access them.

use libdd_ipc::platform::{
    FileBackedHandle, NamedShmHandle, OwnedFileHandle, PlatformHandle, ProcessIdentityGuard,
    ShmHandle,
};
use libdd_ipc::{HANDLE_SUFFIX_SIZE, SeqpacketConn, SeqpacketListener};
use std::ffi::CString;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::panic::{catch_unwind, panic_any};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Security::{
    GetTokenInformation, ImpersonateAnonymousToken, IsWellKnownSid, RevertToSelf, TOKEN_QUERY,
    TOKEN_USER, TokenUser, WinAnonymousSid,
};
use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

/// Impersonates ANONYMOUS LOGON on the current thread until dropped.
struct Anonymous;

impl Anonymous {
    fn impersonate() -> Self {
        assert_ne!(unsafe { ImpersonateAnonymousToken(GetCurrentThread()) }, 0);
        assert!(impersonating_anonymous());
        Self
    }
}

impl Drop for Anonymous {
    fn drop(&mut self) {
        unsafe { RevertToSelf() };
    }
}

/// Whether the thread token is ANONYMOUS LOGON; false when the thread does not impersonate.
fn impersonating_anonymous() -> bool {
    let mut token: HANDLE = 0;
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
        return false;
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token as RawHandle) };
    let mut buffer = [0u64; 64];
    let mut len = 0;
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer) as u32,
            &mut len,
        )
    };
    assert_ne!(ok, 0, "{}", io::Error::last_os_error());
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    unsafe { IsWellKnownSid(user.User.Sid, WinAnonymousSid) != 0 }
}

fn unique_name(kind: &str) -> String {
    format!("libdd-ipc-impersonation-{kind}-{}", std::process::id())
}

#[test]
fn guards_nest_and_restore_impersonation() {
    let _anonymous = Anonymous::impersonate();
    {
        let _outer = ProcessIdentityGuard::enter();
        assert!(!impersonating_anonymous());
        {
            // The thread no longer has a token: the inner guard has nothing to restore.
            let _inner = ProcessIdentityGuard::enter();
            assert!(!impersonating_anonymous());
        }
        assert!(!impersonating_anonymous());
    }
    assert!(impersonating_anonymous());
}

#[test]
fn guard_restores_impersonation_on_unwind() {
    let _anonymous = Anonymous::impersonate();
    let unwound = catch_unwind(|| {
        let _identity = ProcessIdentityGuard::enter();
        panic_any(impersonating_anonymous());
    });
    assert_eq!(unwound.unwrap_err().downcast_ref::<bool>(), Some(&false));
    assert!(impersonating_anonymous());
}

#[test]
fn named_shm_is_shared_with_the_process_identity() {
    let by_process = CString::new(format!("/{}", unique_name("process"))).unwrap();
    let by_client = CString::new(format!("/{}", unique_name("client"))).unwrap();
    let _sidecar_owned = NamedShmHandle::create(by_process.clone(), 4096).unwrap();

    let anonymous = Anonymous::impersonate();
    NamedShmHandle::open(&by_process).unwrap().map().unwrap();
    let _client_owned = NamedShmHandle::create(by_client.clone(), 4096).unwrap();
    assert!(impersonating_anonymous());
    drop(anonymous);

    // The sidecar opens what an impersonating client created.
    NamedShmHandle::open(&by_client).unwrap().map().unwrap();
}

#[test]
fn handles_are_sent_while_impersonating() {
    let (server, client) = SeqpacketConn::socketpair().unwrap();
    let _anonymous = Anonymous::impersonate();
    // As when sending a trace: allocate a section and duplicate it into the peer.
    let shm: PlatformHandle<OwnedFileHandle> = ShmHandle::new(4096).unwrap().into();
    for _ in 0..2 {
        client
            .send_raw_blocking(vec![7], &[shm.as_raw_handle()])
            .unwrap();
        let mut buffer = vec![0; 1 + HANDLE_SUFFIX_SIZE];
        let (len, handles) = server.recv_raw_blocking(&mut buffer).unwrap();
        assert_eq!((len, handles.len()), (1, 1));
    }
    assert!(impersonating_anonymous());
}

fn accept(listener: &SeqpacketListener) -> SeqpacketConn {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.try_accept() {
            Ok(conn) => return conn,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) => panic!("accept failed: {e}"),
        }
    }
}

#[test]
fn pipes_are_shared_with_the_process_identity() {
    let name = format!(r"\\.\pipe\{}", unique_name("pipe"));
    let _anonymous = Anonymous::impersonate();
    // Like a client binding the pipe that it hands over to the sidecar daemon.
    let listener = SeqpacketListener::bind(&name).unwrap();
    std::thread::scope(|scope| {
        // New threads do not impersonate, like the daemon. Owning the listener lets a failed
        // accept close the pipe, so that the client cannot wait forever.
        let daemon = scope.spawn(move || [accept(&listener), accept(&listener)]);
        // The second client needs the pipe instance the daemon creates on the first accept.
        let clients = [&name, &name].map(|name| SeqpacketConn::connect(name).unwrap());
        for (client, server) in clients.iter().zip(&daemon.join().unwrap()) {
            client.send_raw_blocking(vec![7], &[]).unwrap();
            let mut buffer = vec![0; 1 + HANDLE_SUFFIX_SIZE];
            assert_eq!(server.recv_raw_blocking(&mut buffer).unwrap().0, 1);
        }
    });
    assert!(impersonating_anonymous());
}
