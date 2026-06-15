// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_signal_safe_http_client::{
    io::{embedded_io_async::Read, embedded_nal_async::Dns},
    request::Method,
    HttpClient,
};
use sha2::{Digest, Sha256};

use super::{downloads::Sha256Download, linux_net::RustixTcpConnector, logger::Logger};

const PROGRESS_INTERVAL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub(super) enum VerifyError {
    Http,
    UnexpectedStatus,
    MissingLength,
    TooLarge,
    LengthMismatch,
    ChecksumMismatch,
}

impl VerifyError {
    pub(super) const fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http error",
            Self::UnexpectedStatus => "unexpected HTTP status",
            Self::MissingLength => "missing content-length",
            Self::TooLarge => "download too large",
            Self::LengthMismatch => "content-length mismatch",
            Self::ChecksumMismatch => "sha256 mismatch",
        }
    }
}

pub(super) async fn verify_downloads<D>(
    tcp: &RustixTcpConnector,
    dns: &D,
    downloads: &[Sha256Download<'_>],
) -> Result<(), VerifyError>
where
    D: Dns + Sync,
{
    let mut response_buffer = [0_u8; 4096];
    let mut body_buffer = [0_u8; 8192];
    let mut index = 0_usize;

    for download in downloads {
        index += 1;
        Logger::download_start(index, downloads.len(), download.url);

        let mut client = HttpClient::new(tcp, dns);
        verify_download(
            &mut client,
            download,
            &mut response_buffer,
            &mut body_buffer,
        )
        .await?;
    }

    Ok(())
}

async fn verify_download<D>(
    client: &mut HttpClient<'_, RustixTcpConnector, D>,
    download: &Sha256Download<'_>,
    response_buffer: &mut [u8],
    body_buffer: &mut [u8],
) -> Result<(), VerifyError>
where
    D: Dns + Sync,
{
    Logger::line("request: GET");
    let mut request = client
        .request(Method::GET, download.url)
        .await
        .map_err(|_| VerifyError::Http)?;
    Logger::line("request: send");
    let response = request
        .send(response_buffer)
        .await
        .map_err(|_| VerifyError::Http)?;
    Logger::http_status(response.status.0);
    if response.status.0 != 200 {
        return Err(VerifyError::UnexpectedStatus);
    }

    let expected_len = response.content_length.ok_or(VerifyError::MissingLength)?;
    Logger::field_usize("content-length", expected_len);
    if expected_len > download.max_len {
        return Err(VerifyError::TooLarge);
    }

    let mut body = response.body().reader();
    let mut hasher = Sha256::new();
    let mut total = 0_usize;
    let mut next_progress = PROGRESS_INTERVAL_BYTES;

    loop {
        let read = body
            .read(body_buffer)
            .await
            .map_err(|_| VerifyError::Http)?;
        if read == 0 {
            break;
        }

        total = total.checked_add(read).ok_or(VerifyError::TooLarge)?;
        if total > download.max_len {
            return Err(VerifyError::TooLarge);
        }

        hasher.update(&body_buffer[..read]);

        if total >= next_progress || total == expected_len {
            Logger::progress(total, expected_len);
            while next_progress <= total {
                let Some(next) = next_progress.checked_add(PROGRESS_INTERVAL_BYTES) else {
                    next_progress = usize::MAX;
                    break;
                };
                next_progress = next;
            }
        }
    }

    if total != expected_len {
        return Err(VerifyError::LengthMismatch);
    }

    let actual = hasher.finalize();
    Logger::sha256(&actual);
    if !digest_matches(&actual, &download.sha256) {
        return Err(VerifyError::ChecksumMismatch);
    }

    Logger::line("download verified");
    Ok(())
}

fn digest_matches(actual: &[u8], expected: &[u8; 32]) -> bool {
    if actual.len() != expected.len() {
        return false;
    }

    let mut diff = 0_u8;
    for i in 0..expected.len() {
        diff |= actual[i] ^ expected[i];
    }

    diff == 0
}
