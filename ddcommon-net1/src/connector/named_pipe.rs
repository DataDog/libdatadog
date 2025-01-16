// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};

/// Windows Named Pipe
/// https://docs.microsoft.com/en-us/windows/win32/ipc/named-pipes
///
/// The form a windows named pipe path is either local to the computer:
/// \\.\pipe\pipename
/// or targeting a remote server
/// \\ServerName\pipe\pipename
///
/// Build a URI from a Path representing a named pipe
/// `path` - named pipe path. ex: \\.\pipe\pipename
pub fn named_pipe_path_to_uri(path: &Path) -> Result<hyper::Uri, hyper::http::Error> {
    let path = hex::encode(path.as_os_str().to_str().unwrap());
    hyper::Uri::builder()
        .scheme("windows")
        .authority(path)
        .path_and_query("")
        .build()
}

pub fn named_pipe_path_from_uri(uri: &hyper::Uri) -> anyhow::Result<PathBuf> {
    if uri.scheme_str() != Some("windows") {
        return Err(super::errors::Error::InvalidUrl.into());
    }

    let path = hex::decode(
        uri.authority()
            .ok_or(super::errors::Error::InvalidUrl)?
            .as_str(),
    )
    .map_err(|_| super::errors::Error::InvalidUrl)?;

    match String::from_utf8(path) {
        Ok(s) => Ok(PathBuf::from(s.as_str())),
        _ => Err(super::errors::Error::InvalidUrl.into()),
    }
}

#[test]
fn test_encode_named_pipe_for_local_server() {
    let expected_path = r"\\.\pipe\pipename".as_ref();
    let uri = named_pipe_path_to_uri(expected_path).unwrap();
    assert_eq!(uri.scheme_str(), Some("windows"));

    let actual_path = named_pipe_path_from_uri(&uri).unwrap();
    assert_eq!(actual_path.as_path(), Path::new(expected_path))
}

#[test]
fn test_encode_named_pipe_for_remote_server() {
    let expected_path = r"\\servername\pipe\pipename".as_ref();
    let uri = named_pipe_path_to_uri(expected_path).unwrap();
    assert_eq!(uri.scheme_str(), Some("windows"));

    let actual_path = named_pipe_path_from_uri(&uri).unwrap();
    assert_eq!(actual_path.as_path(), Path::new(expected_path));
}
