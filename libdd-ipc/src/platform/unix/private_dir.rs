// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Directories for IPC artifacts that no other user can write to.
//!
//! The sockets and shared-memory fallback files this crate creates live under a shared,
//! world-writable path - `/tmp` - and have names any local process can predict. A directory
//! that every uid must be able to write to cannot protect them: nothing stops another user
//! *creating* a file there first, and getting there first with a socket or segment named after
//! somebody else's uid is enough to receive that user's data. The sticky bit does not help
//! either; it restricts `unlink`/`rename`, not creation, and it exempts the directory's own
//! owner.
//!
//! So each uid gets a directory it owns and only it can write to. Anything that does not match
//! that description gets one repair attempt - discarded and recreated, never adjusted in place
//! ([`ensure`]) - and is otherwise refused.
//!
//! Because nothing foreign can exist inside a directory that passes [`verify`], one check
//! covers every file kept there.
//!
//! # Why one check is enough
//!
//! Nothing foreign can appear *inside* a directory that only its owner may write to, so one
//! check covers every file kept there, for as long as it is the same directory.

use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::prelude::PermissionsExt;
use std::{fs, io, path::Path};

/// Create `path` with `mode` if it does not exist, then [`verify`] it - repairing it once if
/// what is there is not acceptable.
///
/// `mode` should deny group and other *write*; `verify` rejects anything looser. Give others
/// `--x` (0o711) only when a process running as a different uid has to traverse to a file
/// inside, and never `r` or `w`.
///
/// # Repair
///
/// A directory that fails [`verify`] is discarded and recreated rather than adjusted: if it was
/// ever writable by another user, whatever is inside it may have been planted, so a `chmod`
/// would keep the problem while hiding it. Recreating from scratch is the only repair that
/// leaves nothing behind.
///
/// The repair succeeds exactly when the problem was ours to begin with - our own directory left
/// with the wrong mode, or a stale file where a directory belongs. It cannot succeed when
/// another user owns the entry, because removing it in a sticky parent needs ownership, and
/// that is precisely the case where proceeding would be dangerous. So one attempt is made and
/// the result decides: repaired, or refused.
///
/// This is the write path. Clients must use [`verify_owned_by`] instead and never repair -
/// deleting the directory a listener is serving from would be a denial of service dressed up as
/// a fix.
pub fn ensure(path: &Path, mode: u32) -> io::Result<()> {
    // The parent dir of `path` is trusted by the owner that nobody unauthorized may write to it.
    fn create(path: &Path, mode: u32) -> io::Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(mode)
            .create(path)
    }

    // `AlreadyExists` here means something is at the path that is *not* a directory - an
    // existing directory makes a recursive create succeed. Tolerate it so that `verify` gets to
    // classify it and the repair below gets to deal with it, instead of failing on the spot.
    if let Err(e) = create(path, mode) {
        if e.kind() != io::ErrorKind::AlreadyExists {
            return Err(e);
        }
    }
    // Enforced after verification, never before: chmod is only safe on a directory just
    // confirmed to be ours. `DirBuilder::mode` alone does not settle it, because mkdir masks
    // the mode with the umask - under one that strips owner bits (`umask 0700` turns 0700 into
    // 0000) we would create a directory we cannot then use. `verify` would accept it, since it
    // refuses *exposure*, not uselessness, and the failure would surface later as an
    // unexplained EACCES from the socket bind or the lock file.
    //
    // A too-*permissive* mode is not repaired this way - `verify` rejects group/other write
    // outright, so those go through the discard-and-recreate path below instead. Anything that
    // was world-writable may already have had something planted in it.
    fn enforce_mode(path: &Path, mode: u32) -> io::Result<()> {
        if fs::symlink_metadata(path)?.permissions().mode() & 0o7777 == mode {
            return Ok(());
        }
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }

    verify(path).or_else(|first_failure| {
        // One repair attempt, then the answer is final: if the second verify still refuses, the
        // directory is not ours to fix.
        tracing::warn!(
            "Discarding and recreating the IPC directory {}: {first_failure}",
            path.display()
        );

        /// A symlink is unlinked rather than followed: following it would delete somebody else's
        /// directory, which is a far worse outcome than failing to repair.
        fn discard_recursively(path: &Path) -> io::Result<()> {
            if fs::symlink_metadata(path)?.file_type().is_symlink() {
                return fs::remove_file(path);
            }
            match fs::remove_dir_all(path) {
                Ok(()) => Ok(()),
                // Not a directory at all - a stale regular file or socket where ours belongs.
                Err(e) if e.kind() == io::ErrorKind::NotADirectory => fs::remove_file(path),
                Err(e) => Err(e),
            }
        }

        discard_recursively(path).map_err(|e| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{first_failure}, and it could not be removed to start over: {e}"),
            )
        })?;
        create(path, mode)?;
        verify(path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("{e} even after being recreated; refusing to use it"),
            )
        })
    })?;

    enforce_mode(path, mode)
}

/// Check that `path` is a directory owned by this euid that no other user can write to.
pub fn verify(path: &Path) -> io::Result<()> {
    // SAFETY: geteuid() takes no arguments and cannot fail.
    verify_owned_by(path, unsafe { libc::geteuid() })
}

/// Check that `path` is a directory owned by `owner` that no other user can write to.
pub fn verify_owned_by(path: &Path, owner: u32) -> io::Result<()> {
    // symlink_metadata, not metadata: a symlink planted here would silently redirect us to
    // wherever its author chose.
    let md = fs::symlink_metadata(path)?;
    if md.file_type().is_symlink() || !md.is_dir() {
        return Err(refuse(path, "not a directory".to_string()));
    }
    if md.uid() != owner {
        return Err(refuse(
            path,
            format!("owned by uid {}, not {owner}", md.uid()),
        ));
    }
    let mode = md.permissions().mode() & 0o7777;
    if mode & 0o022 != 0 {
        return Err(refuse(
            path,
            format!("writable by other users (mode {mode:o})"),
        ));
    }
    Ok(())
}

fn refuse(path: &Path, reason: String) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("refusing IPC directory {}: {reason}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        #[allow(clippy::unwrap_used)]
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn creates_and_accepts_a_private_directory() {
        let base = tmp();
        let dir = base.path().join("private");
        ensure(&dir, 0o700).expect("must create");
        assert_eq!(
            fs::metadata(&dir).expect("stat").permissions().mode() & 0o777,
            0o700
        );
        ensure(&dir, 0o700).expect("must be idempotent");
    }

    #[test]
    fn accepts_traversable_but_not_writable() {
        let base = tmp();
        let dir = base.path().join("traversable");
        ensure(&dir, 0o711).expect("0711 must be accepted: others may traverse, not write");
    }

    #[test]
    fn refuses_a_directory_others_can_write_to() {
        let base = tmp();
        let dir = base.path().join("loose");
        fs::create_dir(&dir).expect("create");
        for mode in [0o777, 0o733, 0o707, 0o1777] {
            fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).expect("chmod");
            let err = verify(&dir).expect_err(&format!("mode {mode:o} must be refused"));
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        }
    }

    /// `verify` must never mutate. It is the primitive for clients and readers, and discarding
    /// a directory another process is serving from would be a denial of service dressed up as a
    /// fix - only `ensure`, the write path, may repair.
    #[test]
    fn verify_never_repairs() {
        let base = tmp();
        let dir = base.path().join("loose");
        fs::create_dir(&dir).expect("create");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).expect("chmod");
        let in_use = dir.join("in-use.sock");
        fs::write(&in_use, b"a writer's file").expect("plant");

        verify(&dir).expect_err("a permissive directory must be refused");

        assert_eq!(
            fs::metadata(&dir).expect("stat").permissions().mode() & 0o777,
            0o777,
            "verify must leave the directory exactly as it found it"
        );
        assert!(in_use.exists(), "verify must not remove anything");
    }

    #[test]
    fn refuses_a_symlink() {
        let base = tmp();
        let target = base.path().join("target");
        fs::create_dir(&target).expect("create");
        let link = base.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        // Following it would land in a directory whose *path* we never vetted.
        let err = verify(&link).expect_err("a symlink must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn repairs_a_directory_of_ours_left_too_permissive() {
        let base = tmp();
        let dir = base.path().join("ours-but-loose");
        fs::create_dir(&dir).expect("create");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).expect("chmod");
        // Anything already inside may have been planted by whoever could write here, so the
        // repair has to take the contents with it rather than just tighten the mode.
        let planted = dir.join("planted.sock");
        fs::write(&planted, b"not ours").expect("plant");

        ensure(&dir, 0o700).expect("our own directory must be repairable");
        assert_eq!(
            fs::metadata(&dir).expect("stat").permissions().mode() & 0o777,
            0o700,
            "the repaired directory must have the requested mode"
        );
        assert!(
            !planted.exists(),
            "recreating from scratch must discard anything that was inside"
        );
    }

    #[test]
    fn repairs_a_stale_file_where_the_directory_belongs() {
        let base = tmp();
        let path = base.path().join("stale");
        fs::write(&path, b"leftover").expect("write");
        ensure(&path, 0o700).expect("a stale file of ours must be replaced");
        assert!(fs::metadata(&path).expect("stat").is_dir());
    }

    /// A directory we own but cannot use is repaired by chmod, not by deletion: `mkdir` masks
    /// its mode with the umask, so one that strips owner bits leaves exactly this state, and
    /// nothing in the ownership check notices - it refuses exposure, not uselessness.
    #[test]
    fn repairs_an_owned_directory_with_an_unusable_mode() {
        let base = tmp();
        let dir = base.path().join("unusable");
        fs::create_dir(&dir).expect("create");
        let canary = dir.join("keep-me");
        fs::write(&canary, b"x").expect("write");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).expect("chmod");

        ensure(&dir, 0o700).expect("a directory we own must be made usable, not refused");

        assert_eq!(
            fs::metadata(&dir).expect("stat").permissions().mode() & 0o777,
            0o700,
            "the mode must be enforced, not merely requested at mkdir time"
        );
        assert!(
            canary.exists(),
            "a mode-only problem must be chmod-ed, not resolved by discarding the contents"
        );
    }

    #[test]
    fn fails_when_the_repair_cannot_be_carried_out() {
        // The failure is staged by removing a write permission, and root is not subject to
        // those: the repair would simply succeed and the assertion below would be wrong about
        // why. Nothing to test here rather than something to fail.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!(
                "skipping fails_when_the_repair_cannot_be_carried_out: \
                 running as root, which bypasses the permission check it stages"
            );
            return;
        }

        let base = tmp();
        let parent = base.path().join("locked");
        fs::create_dir(&parent).expect("create");
        let dir = parent.join("loose");
        fs::create_dir(&dir).expect("create");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).expect("chmod");
        // Stands in for the case that actually matters - a directory another uid owns, which we
        // cannot remove - by taking away the write permission the removal needs. Staging that
        // case for real would require privileges.
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).expect("chmod");

        let err = ensure(&dir, 0o700).expect_err("an unrepairable directory must fail");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            err.to_string()
                .contains("could not be removed to start over"),
            "the error must say the repair was attempted and why it failed, got: {err}"
        );

        // Restore write permission so the TempDir can clean itself up.
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).expect("chmod");
    }

    #[test]
    fn refuses_a_directory_owned_by_someone_else() {
        let base = tmp();
        let dir = base.path().join("foreign");
        ensure(&dir, 0o700).expect("create");
        // Stands in for a directory another uid created: we cannot chown without privileges,
        // so assert through the explicit-owner entry point instead.
        // SAFETY: geteuid() takes no arguments and cannot fail.
        let other = unsafe { libc::geteuid() } + 1;
        let err = verify_owned_by(&dir, other).expect_err("foreign ownership must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }
}
