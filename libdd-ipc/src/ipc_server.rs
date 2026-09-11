// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{PeerCredentials, SeqpacketConn};
use std::io;

pub struct OwnedServerConn {
    connection: crate::AsyncConn,
    peer: PeerCredentials,
}

impl OwnedServerConn {
    pub fn new(conn: SeqpacketConn) -> io::Result<Self> {
        let peer = conn.peer_credentials().unwrap_or_default();
        let connection = conn.into_async_conn()?;
        Ok(Self { connection, peer })
    }

    /// Construct from an already-async connection and a known peer. Useful for callers that have
    /// already wrapped the fd (and for tests).
    pub fn from_async(connection: crate::AsyncConn, peer: PeerCredentials) -> Self {
        Self { connection, peer }
    }

    pub fn async_conn(&self) -> &crate::AsyncConn {
        &self.connection
    }

    pub fn peer(&self) -> &PeerCredentials {
        &self.peer
    }
}
