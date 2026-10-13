// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native file capability backed by `tokio::fs`.

use core::future::Future;
use std::io;

use bytes::Bytes;
use libdd_capabilities::file::{FileCapability, FileError, FileMetadata};
use libdd_capabilities::maybe_send::MaybeSend;

#[derive(Clone, Debug)]
pub struct NativeFileCapability;

impl FileCapability for NativeFileCapability {
    fn new() -> Self {
        Self
    }

    fn read(&self, path: &str) -> impl Future<Output = Result<Bytes, FileError>> + MaybeSend {
        let path = path.to_owned();
        async move {
            match tokio::fs::read(&path).await {
                Ok(bytes) => Ok(Bytes::from(bytes)),
                Err(e) => Err(map_io_error(e, &path)),
            }
        }
    }

    fn write(
        &self,
        path: &str,
        contents: Bytes,
    ) -> impl Future<Output = Result<(), FileError>> + MaybeSend {
        let path = path.to_owned();
        async move {
            tokio::fs::write(&path, &contents)
                .await
                .map_err(|e| map_io_error(e, &path))
        }
    }

    fn metadata(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<FileMetadata, FileError>> + MaybeSend {
        let path = path.to_owned();
        async move {
            let m = tokio::fs::metadata(&path)
                .await
                .map_err(|e| map_io_error(e, &path))?;
            Ok(FileMetadata {
                size: m.len(),
                inode: platform_inode(&m),
                is_file: m.is_file(),
                is_dir: m.is_dir(),
            })
        }
    }

    fn exists(&self, path: &str) -> impl Future<Output = Result<bool, FileError>> + MaybeSend {
        let path = path.to_owned();
        async move {
            match tokio::fs::metadata(&path).await {
                Ok(_) => Ok(true),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(map_io_error(e, &path)),
            }
        }
    }
}

fn map_io_error(e: io::Error, path: &str) -> FileError {
    match e.kind() {
        io::ErrorKind::NotFound => FileError::NotFound(path.to_owned()),
        io::ErrorKind::PermissionDenied => FileError::PermissionDenied(path.to_owned()),
        _ => FileError::Io(anyhow::Error::new(e).context(format!("path: {path}"))),
    }
}

#[cfg(unix)]
fn platform_inode(m: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(m.ino())
}

#[cfg(not(unix))]
fn platform_inode(_m: &std::fs::Metadata) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        TempDir::new().expect("tempdir")
    }

    #[tokio::test]
    async fn read_write_roundtrip() {
        let dir = tmp();
        let path = dir.path().join("file.bin");
        let path_str = path.to_str().unwrap();
        let cap = NativeFileCapability;
        cap.write(path_str, Bytes::from_static(b"hello"))
            .await
            .expect("write ok");
        let got = cap.read(path_str).await.expect("read ok");
        assert_eq!(&got[..], b"hello");
    }

    #[tokio::test]
    async fn read_missing_yields_not_found() {
        let dir = tmp();
        let path = dir.path().join("missing.bin");
        let cap = NativeFileCapability;
        let err = cap.read(path.to_str().unwrap()).await.unwrap_err();
        assert!(matches!(err, FileError::NotFound(_)), "got: {err:?}");
    }

    #[tokio::test]
    async fn metadata_reports_size_and_kind() {
        let dir = tmp();
        let path = dir.path().join("f.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"1234567890").unwrap();
        drop(f);
        let cap = NativeFileCapability;
        let m = cap.metadata(path.to_str().unwrap()).await.expect("meta ok");
        assert_eq!(m.size, 10);
        assert!(m.is_file);
        assert!(!m.is_dir);
        #[cfg(unix)]
        assert!(m.inode.is_some());
        #[cfg(not(unix))]
        assert!(m.inode.is_none());
    }

    #[tokio::test]
    async fn exists_true_and_false() {
        let dir = tmp();
        let present = dir.path().join("here");
        std::fs::write(&present, b"").unwrap();
        let absent = dir.path().join("gone");
        let cap = NativeFileCapability;
        assert!(cap.exists(present.to_str().unwrap()).await.unwrap());
        assert!(!cap.exists(absent.to_str().unwrap()).await.unwrap());
    }
}
