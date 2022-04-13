// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::error::Error;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// Creates a new Uri, with the `unix` scheme, and the path to the socket
/// encoded as a hex string, to prevent special characters in the url authority
pub fn socket_path_to_uri(path: &Path) -> Result<hyper::Uri, Box<dyn Error>> {
    let path = hex::encode(path.as_os_str().as_bytes());
    Ok(hyper::Uri::builder()
        .scheme("unix")
        .authority(path)
        .path_and_query("")
        .build()?)
}

pub fn socket_path_from_uri(uri: &hyper::Uri) -> anyhow::Result<PathBuf> {
    if uri.scheme_str() != Some("unix") {
        return Err(crate::errors::Error::InvalidUrl.into());
    }
    let path = hex::decode(
        uri.authority()
            .ok_or(crate::errors::Error::InvalidUrl)?
            .as_str(),
    )
    .map_err(|_| crate::errors::Error::InvalidUrl)?;
    Ok(PathBuf::from(OsString::from_vec(path)))
}

#[test]
fn test_encode_unix_socket_path_absolute() {
    let expected_path = "/path/to/a/socket.sock".as_ref();
    let uri = socket_path_to_uri(expected_path).unwrap();
    assert_eq!(uri.scheme_str(), Some("unix"));

    let actual_path = socket_path_from_uri(&uri).unwrap();
    assert_eq!(actual_path.as_path(), Path::new(expected_path))
}

#[test]
fn test_encode_unix_socket_relative_path() {
    let expected_path = "relative/path/to/a/socket.sock".as_ref();
    let uri = socket_path_to_uri(expected_path).unwrap();
    let actual_path = socket_path_from_uri(&uri).unwrap();
    assert_eq!(actual_path.as_path(), Path::new(expected_path));

    let expected_path = "./relative/path/to/a/socket.sock".as_ref();
    let uri = socket_path_to_uri(expected_path).unwrap();
    let actual_path = socket_path_from_uri(&uri).unwrap();
    assert_eq!(actual_path.as_path(), Path::new(expected_path));
}
