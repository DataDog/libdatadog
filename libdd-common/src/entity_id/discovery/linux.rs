// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::OriginRead;
use alloc::vec::Vec;
use rustix::{fs, io};

pub(super) struct Procfs;

impl OriginRead for Procfs {
    type Error = io::Errno;

    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        read_file(path)
    }

    fn inode(&self, path: &str) -> io::Result<u64> {
        // stat follows the /proc/self/ns/cgroup symlink to the namespace inode.
        Ok(io::retry_on_intr(|| fs::stat(path))?.st_ino)
    }
}

pub(super) fn read_file(path: &str) -> io::Result<Vec<u8>> {
    let fd = io::retry_on_intr(|| {
        fs::open(
            path,
            fs::OFlags::RDONLY | fs::OFlags::CLOEXEC,
            fs::Mode::empty(),
        )
    })?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = io::retry_on_intr(|| io::read(&fd, &mut buffer))?;
        if count == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::{io::Write, os::unix::fs::MetadataExt};

    #[test]
    fn rustix_reads_empty_and_multiple_chunks_and_reports_missing_files() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let path = alloc::string::String::from(file.path().to_str().unwrap());
        assert!(read_file(&path).unwrap().is_empty());
        let bytes = [b'x'; 12_345];
        file.write_all(&bytes).unwrap();
        assert_eq!(read_file(&path).unwrap(), bytes);
        drop(file);
        assert_eq!(read_file(&path), Err(io::Errno::NOENT));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri does not support stat64")]
    fn rustix_inode_lookup_follows_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        std::fs::write(&target, b"contents").unwrap();
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let inode = Procfs.inode(link.to_str().unwrap()).unwrap();
        assert_eq!(inode, std::fs::metadata(&target).unwrap().ino());
        assert_ne!(inode, std::fs::symlink_metadata(&link).unwrap().ino());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri's uname shim leaves bytes uninitialized")]
    fn rustix_hostname_matches_the_kernel_namespace_hostname() {
        let expected = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap();
        assert_eq!(
            super::super::read_hostname().unwrap(),
            expected.strip_suffix('\n').unwrap_or(&expected)
        );
    }
}
