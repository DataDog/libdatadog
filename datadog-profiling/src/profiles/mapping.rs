// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::StringOffset;

/// A representation of a mapping that is an intersection of the Otel and Pprof
/// representations. Omits boolean attributes because Datadog doesn't use them
/// in any way.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Mapping {
    memory_start: u64,
    memory_limit: u64,
    file_offset: u64,
    filename: StringOffset,
    build_id: StringOffset, // missing in Otel, is it made into an attribute?
}

#[allow(clippy::unwrap_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::collections::Table;
    use crate::profiles::ProfileId;

    #[test]
    fn test_mapping() {
        let table = Table::try_with_capacity(4).unwrap();

        let default = table.get(ProfileId::ZERO).unwrap();
        assert_eq!(default, &Mapping::default());
        let id = table.insert(Mapping::default()).unwrap();
    }
}
