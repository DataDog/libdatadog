// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::path::{Path, PathBuf};

pub fn socket_path_to_uri(path: &Path) -> Result<hyper::Uri, hyper::http::Error> {
    let path = hex::encode(path.as_os_str().to_str().unwrap());
    hyper::Uri::builder()
        .scheme("windows")
        .authority(path)
        .path_and_query("")
        .build()
}

pub fn socket_path_from_uri(uri: &hyper::Uri) -> anyhow::Result<PathBuf> {
    if uri.scheme_str() != Some("windows") {
        return Err(super::errors::Error::InvalidUrl.into());
    } 

    let path = hex::decode(
        uri.authority()
            .ok_or(super::errors::Error::InvalidUrl)?
            .as_str(),
    )
    .map_err(|_| super::errors::Error::InvalidUrl)?;
    
    return match String::from_utf8(path) {
        Ok(s) => Ok(PathBuf::from(s.as_str())),
        _ => Err(super::errors::Error::InvalidUrl.into()),
    };
}

#[test]
fn test_encode_named_pipe_for_local_server() {
    let expected_path = r"\\.\pipe\pipename".as_ref();
    let uri = socket_path_to_uri(expected_path).unwrap();
    assert_eq!(uri.scheme_str(), Some("windows"));

    let actual_path = socket_path_from_uri(&uri).unwrap();
    assert_eq!(actual_path.as_path(), Path::new(expected_path))
}

#[test]
fn test_encode_named_pipe_for_remote_server() {
    let expected_path = r"\\servername\pipe\pipename".as_ref();
    let uri = socket_path_to_uri(expected_path).unwrap();
    assert_eq!(uri.scheme_str(), Some("windows"));
    
    let actual_path = socket_path_from_uri(&uri).unwrap();
    assert_eq!(actual_path.as_path(), Path::new(expected_path));
}