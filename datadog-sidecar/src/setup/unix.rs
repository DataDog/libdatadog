// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::LazyLock;
use std::{
    env, fs, io,
    os::unix::fs::{FileTypeExt, MetadataExt},
    os::unix::prelude::PermissionsExt,
    path::{Path, PathBuf},
};

use crate::primary_sidecar_identifier;
use crate::setup::Liaison;
use libdd_ipc::platform::locks::FLock;
use libdd_ipc::platform::private_dir;
use libdd_ipc::{SeqpacketConn, SeqpacketListener};

use tracing::{trace, warn};

pub type IpcClient = SeqpacketConn;
pub type IpcServer = SeqpacketListener;

/// How a liaison's socket is reached, and therefore how it is protected.
///
/// `bind(2)` creates a socket with `0o777 & ~umask`, so a socket's own mode says nothing about
/// who put it there. What distinguishes these two is which fact a client can check instead.
#[derive(Debug, Clone, Copy)]
enum SocketAccess {
    /// The socket name embeds `geteuid()` and it lives in a directory private to that uid, so
    /// no other user can compute the name or write beside it. The *directory* is the evidence.
    OwnerOnly,
    /// Thread mode: the listener is a thread inside a PHP master whose workers may have dropped
    /// to a different uid, and they still have to reach it. That rules out any uid-keyed
    /// location - on macOS `env::temp_dir()` is itself per uid, resolving through
    /// `_CS_DARWIN_USER_TEMP_DIR` to /var/folders/<hash>/T - so the socket goes straight into
    /// /tmp under a name derived only from the listener's pid. The *file's owner* is the
    /// evidence there, and the sticky bit is what makes that evidence hold still.
    ListenerOwned { listener_pid: u32 },
}

/// Mode for a private liaison directory. Denies group and other everything.
const PRIVATE_DIR_MODE: u32 = 0o700;

const ROOT_UID: u32 = 0;

impl SocketAccess {
    /// Mode for the socket itself, set explicitly because `bind(2)` would otherwise leave it at
    /// the mercy of the host umask - which under the usual 022 denies a different-uid worker
    /// the write permission `connect(2)` requires.
    fn socket_mode(self) -> u32 {
        match self {
            SocketAccess::OwnerOnly => 0o600,
            SocketAccess::ListenerOwned { .. } => 0o666,
        }
    }

    /// Prepare the directory the socket is about to be bound in.
    fn prepare_dir(self, dir: &Path) -> io::Result<()> {
        match self {
            SocketAccess::OwnerOnly => private_dir::ensure(dir, PRIVATE_DIR_MODE),
            // /tmp is shared by definition and owned by root. Taking it over is neither
            // possible nor desirable; the socket defends itself instead.
            SocketAccess::ListenerOwned { .. } => Ok(()),
        }
    }

    /// Check, before binding, that anything already at `socket_path` is ours to adopt or
    /// remove. In a shared directory it could be anybody's.
    fn verify_before_bind(self, socket_path: &Path) -> io::Result<()> {
        match self {
            // The private directory already guarantees it: nobody else could have put it there.
            SocketAccess::OwnerOnly => Ok(()),
            // Refuse rather than evict. Root could unlink a stranger's file in /tmp, but
            // declining to start is the lesser harm, and it is visible to an operator.
            SocketAccess::ListenerOwned { .. } => {
                verify_socket_owner(socket_path, primary_sidecar_identifier())
            }
        }
    }

    /// Check, from a client, that whatever sits at `socket_path` was put there by the listener
    /// and not by someone who got there first.
    fn verify_before_connect(self, socket_path: &Path) -> io::Result<()> {
        match self {
            SocketAccess::OwnerOnly => {
                // The directory contains the socket, so this covers it: only the owner may
                // create anything there. Had someone else got there first the listener would
                // have refused the directory and never bound, and this is a client declining
                // to talk to whatever was left behind.
                let dir = socket_path.parent().unwrap_or_else(|| Path::new("/"));
                private_dir::verify_owned_by(dir, primary_sidecar_identifier())
            }
            // A dead listener is not a safe listener: the socket file outlives the process
            // that bound it, so with no uid to check against there is nothing telling its
            // owner apart from anyone else who may have taken the name since.
            SocketAccess::ListenerOwned { listener_pid } => {
                match libdd_ipc::platform::process::effective_uid_of(listener_pid) {
                    Some(expected) => verify_socket_owner(socket_path, expected),
                    None => Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "refusing the sidecar socket {}: listener pid {listener_pid} is \
                             gone, so its owner cannot be established",
                            socket_path.display()
                        ),
                    )),
                }
            }
        }
    }
}

/// Refuse a socket that the expected listener did not create.
///
/// The socket sits in /tmp, where any user may create a file, so its ownership is the only evidence
/// of who put it there - and unlike the abstract socket, there is no post-connect alternative: the
/// macOS handshake transfers a socketpair the *client* created, so client-side peer credentials
/// describe the client.
///
/// Checking with `stat` and then calling `connect` is not the race it looks like. /tmp is sticky
/// (`S_ISVTX`), which restricts unlink and rename to the file's owner, the directory's owner or
/// root - so no unprivileged attacker can swap a listener-owned socket out from under us in
/// between. Sticky never prevented *creation*, which is exactly why it was not enough to protect
/// a directory, but preventing replacement is precisely what is needed here.
fn verify_socket_owner(path: &Path, expected_uid: u32) -> io::Result<()> {
    // symlink_metadata, not metadata: a symlink here would send our `connect` somewhere its
    // author chose while we inspected the target's ownership instead of the link's.
    let md = fs::symlink_metadata(path)?;
    if md.file_type().is_symlink() || !md.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Refusing to use {}: it is not a socket, so the listener did not bind it",
                path.display()
            ),
        ));
    }
    let owner = md.uid();
    if owner != expected_uid && owner != ROOT_UID {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Refusing the sidecar socket {}: owned by uid {owner}, but the listener runs as uid {expected_uid}",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Give the bound socket its intended mode.
///
/// Only reporting it informationally; the socket listener still checks for authentication.
fn set_socket_mode(path: &Path, mode: u32) {
    if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(mode)) {
        warn!(
            "Could not set mode {mode:o} on the sidecar socket {}: {e}",
            path.display()
        );
    }
}

pub struct SharedDirLiaison {
    socket_path: PathBuf,
    lock_path: PathBuf,
    access: SocketAccess,
}

impl Liaison for SharedDirLiaison {
    fn connect_to_server(&self) -> io::Result<SeqpacketConn> {
        self.access.verify_before_connect(&self.socket_path)?;
        SeqpacketConn::connect(&self.socket_path)
    }

    fn attempt_listen(&self) -> io::Result<Option<SeqpacketListener>> {
        let dir = self.socket_path.parent().unwrap_or_else(|| Path::new("/"));
        self.access.prepare_dir(dir)?;

        let _g = match FLock::try_rw_lock(&self.lock_path) {
            Ok(lock) => lock,
            // Failing to acquire the lock means another process is currently creating
            // the socket; the caller then connects to it via connect_to_server(). This
            // is normal under concurrent process startup.
            Err(err) => {
                trace!("another process is creating the sidecar socket");
                return Err(err);
            }
        };

        // symlink_metadata rather than exists(): a *dangling* symlink reads as absent, and
        // `bind` would then follow it and create the socket wherever its author pointed it.
        if fs::symlink_metadata(&self.socket_path).is_ok() {
            self.access.verify_before_bind(&self.socket_path)?;
            // if socket is already listening, then creating listener is not available
            if libdd_ipc::platform::sockets::is_listening(&self.socket_path)? {
                trace!(
                    "The sidecar's socket is already listening ({})",
                    self.socket_path.as_path().display()
                );
                return Ok(None);
            }
            fs::remove_file(&self.socket_path)?;
        }
        let listener = SeqpacketListener::bind(&self.socket_path)?;
        set_socket_mode(&self.socket_path, self.access.socket_mode());
        Ok(Some(listener))
    }

    fn bound_files(&self) -> Vec<PathBuf> {
        vec![self.socket_path.clone(), self.lock_path.clone()]
    }

    fn ipc_shared() -> Self {
        Self::new_default_location()
    }

    fn ipc_per_process() -> Self {
        // The pid alone would not be unique over time - a stale socket left by a dead process
        // that happened to hold this pid would be adopted as ours.
        static PROCESS_RANDOM_ID: LazyLock<u16> = LazyLock::new(rand::random);

        Self::at(
            Self::private_dir_path(),
            &format!(
                "libdd.{}@proc{}-{}.sock",
                crate::sidecar_version!(),
                std::process::id(),
                *PROCESS_RANDOM_ID
            ),
            SocketAccess::OwnerOnly,
        )
    }
}

impl SharedDirLiaison {
    /// A liaison for the socket named `socket_basename` inside `base_dir`.
    fn at(base_dir: PathBuf, socket_basename: &str, access: SocketAccess) -> Self {
        Self {
            socket_path: base_dir.join(socket_basename),
            lock_path: base_dir.join(socket_basename).with_extension("sock.lock"),
            access,
        }
    }

    pub fn new<P: AsRef<Path>>(base_dir: P) -> Self {
        Self::at(
            base_dir.as_ref().to_path_buf(),
            &format!(
                "libdd.{}@{}.sock",
                crate::sidecar_version!(),
                primary_sidecar_identifier()
            ),
            SocketAccess::OwnerOnly,
        )
    }

    pub fn new_default_location() -> Self {
        Self::new(Self::private_dir_path())
    }

    /// The directory for this uid's sidecar sockets.
    ///
    /// One per euid, rather than one shared directory, so that it can be private: a directory
    /// every uid must be able to write to cannot stop any of them planting a socket where
    /// another's clients will look for it. Per euid rather than per anything finer, so that the
    /// count stays bounded and there is nothing to clean up.
    fn private_dir_path() -> PathBuf {
        env::temp_dir().join(format!("libdatadog-{}", primary_sidecar_identifier()))
    }

    /// The liaison for an in-process (thread mode) listener, named after the listener's pid.
    ///
    /// This one lives in /tmp rather than in the per-uid directory, because its clients are the
    /// listener's own workers and those may have dropped to another uid - a stock php-fpm keeps
    /// its master as root and runs its pools as somebody else. Every uid-keyed location is
    /// therefore unusable, including `env::temp_dir()`, which on macOS resolves through
    /// `_CS_DARWIN_USER_TEMP_DIR` to a per-uid /var/folders/<hash>/T. /tmp is the one path a
    /// root master and a dropped worker are guaranteed to spell the same way.
    ///
    /// Anyone may create a file in /tmp, so this trades the private directory's guarantee for a
    /// check on the socket's own ownership - see [`verify_socket_owner`]. A hostile user can
    /// still take the name first and thereby keep the listener from starting; that is a denial
    /// of service, in a namespace shared with every other user on the host, and it is visible
    /// as a refusal rather than silently served.
    pub fn ipc_for_pid(pid: u32) -> Self {
        Self::at(
            PathBuf::from("/tmp"),
            &format!("libdd.{}@pid{pid}.sock", crate::sidecar_version!()),
            SocketAccess::ListenerOwned { listener_pid: pid },
        )
    }

    /// The filesystem socket path this liaison binds/connects to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Default for SharedDirLiaison {
    fn default() -> Self {
        Self::ipc_per_process()
    }
}

#[cfg(target_os = "linux")]
// Important note:
// Never put any runtime data which both the sidecar and the client processes must see onto disk.
// In particular, when using different mount namespaces, but a shared network namespace, the
// processes don't necessarily see the same things.
mod linux {
    use std::{io, path::PathBuf};

    use spawn_worker::getpid;

    use libdd_ipc::platform;
    use libdd_ipc::{SeqpacketConn, SeqpacketListener};

    use super::Liaison;

    /// Which uid must own the listener for a client to trust what it connected to.
    ///
    /// An abstract socket has no ownership model: we have to rely on `SO_PEERCRED`.
    #[derive(Debug, Clone, Copy)]
    enum ExpectedServer {
        /// The socket name embeds `geteuid()`, so only a process with that uid has any business
        /// listening on it.
        OwnUid,
        /// Thread mode: the name embeds the *pid* of an in-process listener, which may well be
        /// running as a different uid than this client - a root master serving workers that
        /// dropped privileges. The uid therefore comes from the process table.
        ListenerPid(u32),
    }

    impl ExpectedServer {
        fn uid(self) -> Option<u32> {
            match self {
                ExpectedServer::OwnUid => Some(crate::primary_sidecar_identifier()),
                // `None` means the pid is gone - which is not the same as nothing being
                // there, since the socket name outlives the process. The caller refuses.
                ExpectedServer::ListenerPid(pid) => platform::process::effective_uid_of(pid),
            }
        }
    }

    pub struct AbstractUnixSocketLiaison {
        path: PathBuf,
        expected_server: ExpectedServer,
    }
    pub type DefaultLiason = AbstractUnixSocketLiaison;

    impl Liaison for AbstractUnixSocketLiaison {
        fn connect_to_server(&self) -> io::Result<SeqpacketConn> {
            let conn = platform::sockets::connect_abstract(&self.path)?;
            // Before a single byte goes out: prove we are talking to our own sidecar and not to
            // whoever got to this name first.
            verify_server(&conn, self.expected_server.uid(), &self.path)?;
            Ok(conn)
        }

        fn attempt_listen(&self) -> io::Result<Option<SeqpacketListener>> {
            match platform::sockets::bind_abstract(&self.path) {
                Ok(l) => Ok(Some(l)),
                Err(ref e) if e.kind() == io::ErrorKind::AddrInUse => Ok(None),
                Err(err) => Err(err),
            }
        }

        fn ipc_shared() -> AbstractUnixSocketLiaison {
            let path = PathBuf::from(format!(
                concat!("libdatadog/", crate::sidecar_version!(), "@{}.sock"),
                crate::primary_sidecar_identifier()
            ));
            Self {
                path,
                expected_server: ExpectedServer::OwnUid,
            }
        }

        fn ipc_per_process() -> AbstractUnixSocketLiaison {
            let path = PathBuf::from(format!(
                concat!("libdatadog/", crate::sidecar_version!(), ".{}.sock"),
                getpid()
            ));
            Self {
                path,
                expected_server: ExpectedServer::OwnUid,
            }
        }
    }

    /// Whether `uid` is one we are willing to be served by.
    ///
    /// uid 0 is accepted alongside `expected`: root can read this process's memory regardless,
    /// so refusing its socket protects nothing.
    fn server_uid_is_acceptable(uid: u32, expected: u32) -> bool {
        uid == expected || uid == 0
    }

    /// Crash-handler-safe form of the check below: no allocation, no formatting.
    pub fn server_is_acceptable(fd: std::os::fd::RawFd) -> bool {
        // SAFETY: geteuid() takes no arguments and cannot fail.
        let euid = unsafe { libc::geteuid() };
        platform::sockets::get_peer_credentials(fd)
            .is_ok_and(|cred| server_uid_is_acceptable(cred.uid, euid))
    }

    /// Refuse a listener that is not owned by the uid that should have created it, with a
    /// message naming what was found - which a crash handler cannot afford, hence the split.
    pub(super) fn verify_server(
        conn: &SeqpacketConn,
        expected: Option<u32>,
        path: &std::path::Path,
    ) -> io::Result<()> {
        let Some(expected) = expected else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing the sidecar at @{}: the listener process is gone, so there is \
                     nothing to check the server against. Another user may hold the name now.",
                    path.display()
                ),
            ));
        };
        let cred = conn.peer_credentials()?;
        if server_uid_is_acceptable(cred.uid, expected) {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing the sidecar at @{}: it is served by pid {} running as uid {}, not \
                 {expected}. Another user may be squatting this socket name.",
                path.display(),
                cred.pid,
                cred.uid
            ),
        ))
    }

    impl AbstractUnixSocketLiaison {
        /// The in-process (thread mode) listener's socket, named after its pid.
        ///
        /// `pid` prefix, because `ipc_shared` keys the same namespace by euid: a master whose
        /// pid happens to equal its euid - pid 33 as uid 33 is perfectly ordinary in a
        /// container - would otherwise compute the shared sidecar's name and the two modes
        /// would silently take over each other's socket.
        pub fn ipc_for_pid(pid: u32) -> Self {
            let path = PathBuf::from(format!(
                concat!("libdatadog/", crate::sidecar_version!(), "@pid{}.sock"),
                pid
            ));
            Self {
                path,
                expected_server: ExpectedServer::ListenerPid(pid),
            }
        }

        /// The abstract socket name this liaison binds/connects to.
        ///
        /// Exposed so the crashtracker collector (in another process) can target the exact same
        /// IPC socket the sidecar listens on.
        pub fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Default for AbstractUnixSocketLiaison {
        fn default() -> Self {
            Self::ipc_shared()
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_abstract_socket_can_connect() {
        let l = AbstractUnixSocketLiaison::ipc_per_process();
        super::tests::basic_liaison_connection_test(&l).unwrap();
    }

    /// An abstract name has no owner, so any local process can bind the one we look for. The
    /// only thing distinguishing our sidecar from a squatter is the uid the kernel reports for
    /// whoever listened, so check that we act on it.
    ///
    /// The refusal case is exercised through `verify_server` directly: staging a listener under
    /// another uid would need privileges, while the uid to compare against is exactly what this
    /// function takes as an argument.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn abstract_client_refuses_a_server_of_another_uid() {
        // Its own name, so this cannot collide with the test above under parallel execution.
        let path = PathBuf::from(format!(
            concat!(
                "libdatadog/",
                crate::sidecar_version!(),
                ".clientauth.{}.sock"
            ),
            getpid()
        ));
        let liaison = AbstractUnixSocketLiaison {
            path: path.clone(),
            expected_server: ExpectedServer::OwnUid,
        };
        let _listener = liaison.attempt_listen().unwrap().unwrap();

        // The normal path: we listened, so we are the server, and connecting must work.
        let conn = liaison.connect_to_server().unwrap();
        let own_uid = crate::primary_sidecar_identifier();

        verify_server(&conn, Some(own_uid), &path).expect("our own uid must be accepted");
        verify_server(&conn, Some(0), &path)
            .expect("root must be accepted: it can read our memory regardless");
        verify_server(&conn, None, &path).expect("an unknown expectation is no basis to refuse");

        let err = verify_server(&conn, Some(own_uid + 1), &path)
            .expect_err("a server running as another uid must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            err.to_string().contains("squatting"),
            "the error must name the likely cause, got: {err}"
        );
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
pub type DefaultLiason = SharedDirLiaison;

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use tempfile::tempdir;

    use libdd_ipc::{SeqpacketConn, SeqpacketListener};

    use super::Liaison;

    /// A socket we created ourselves is exactly what the listener-owned check must accept.
    /// The socket file outlives the process that bound it, so a listener pid that no longer
    /// resolves leaves nothing to check the owner against - and the answer to "cannot verify"
    /// is refuse, not proceed.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn connect_is_refused_when_the_listener_pid_is_gone() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("stale.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();

        // No process can hold this pid: it is above every pid_max in use.
        let access = super::SocketAccess::ListenerOwned {
            listener_pid: u32::MAX,
        };
        let err = access
            .verify_before_connect(&path)
            .expect_err("an unidentifiable listener must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn socket_owner_check_accepts_our_own_socket() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("own.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();

        let me = crate::primary_sidecar_identifier();
        super::verify_socket_owner(&path, me).expect("a socket we own must be accepted");

        // The check also allows root unconditionally, so a foreign owner is only observable as
        // a rejection when we are not root ourselves - which is not true of every CI image.
        if me != 0 {
            super::verify_socket_owner(&path, me + 1)
                .expect_err("a socket owned by another uid must be refused");
        }
    }

    /// Someone who got to the name first need not have left a socket there at all.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn socket_owner_check_rejects_a_non_socket() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("not-a.sock");
        std::fs::write(&path, b"").unwrap();

        super::verify_socket_owner(&path, crate::primary_sidecar_identifier())
            .expect_err("a regular file must be refused, whoever owns it");
    }

    /// The reason the check stats the link and not its target: a symlink we followed would let
    /// its author choose where our connect lands while we inspected the target's ownership.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn socket_owner_check_rejects_a_symlink_to_a_valid_socket() {
        let tmp = tempdir().unwrap();
        let real = tmp.path().join("real.sock");
        let link = tmp.path().join("link.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        super::verify_socket_owner(&link, crate::primary_sidecar_identifier())
            .expect_err("a symlink must be refused even when its target is a socket we own");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_shared_dir_can_connect_to_socket() -> anyhow::Result<()> {
        let tmpdir = tempdir().unwrap();
        let liaison = super::SharedDirLiaison::new(tmpdir.path());
        basic_liaison_connection_test(&liaison).unwrap();
        // socket file will still exist - even if we close everything
        assert!(liaison.socket_path.exists());
        Ok(())
    }

    pub fn basic_liaison_connection_test<T>(liaison: &T) -> Result<(), anyhow::Error>
    where
        T: Liaison,
    {
        {
            let listener: SeqpacketListener = liaison.attempt_listen().unwrap().unwrap();
            // can't listen twice when some listener is active
            assert!(liaison.attempt_listen().unwrap().is_none());

            let client: SeqpacketConn = liaison.connect_to_server().unwrap();
            let srv: SeqpacketConn = listener.try_accept().unwrap();
            client.send_raw_blocking(&mut vec![255], &[]).unwrap();
            let mut buf = [0u8; 4];
            let (n, _) = srv.recv_raw_blocking(&mut buf).unwrap();
            assert_eq!(n, 1);
            assert_eq!(buf[0], 255);
            drop(listener);
            drop(client);
        }
        // sleep to give time to OS to free up resources
        thread::sleep(Duration::from_millis(10));

        // we should be able to open new listener now
        let _listener = liaison.attempt_listen().unwrap().unwrap();
        Ok(())
    }
}
