// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;

pub struct Endpoints {
    pub endpoint_label: InternalStringId,
    pub local_root_span_id_label: InternalStringId,
    pub mappings: FxIndexMap<u64, InternalStringId>,
    pub stats: ProfiledEndpointsStats,
}

impl Endpoints {
    pub fn new() -> Self {
        Self {
            mappings: Default::default(),
            local_root_span_id_label: Default::default(),
            endpoint_label: Default::default(),
            stats: Default::default(),
        }
    }
}

impl Default for Endpoints {
    fn default() -> Self {
        Self::new()
    }
}
