// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]
#![no_std]

mod error;
pub mod protobuf;
mod store;
mod u31;

pub use error::*;
pub use store::*;

#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "prost_impls")]
pub mod prost_impls;

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_alloc::vec::VirtualVec;
    use protobuf::*;

    #[test]
    fn test_strings() {
        let mut storage = VirtualVec::new();
        let mut buffer = Buffer::try_from(&mut storage).unwrap();
        let mut string_table = StringTable::try_new(&mut buffer).unwrap();

        // We always have the empty string.
        assert_eq!(string_table.len(), 1);

        for _ in 0..2 {
            let id = string_table.try_add(&mut buffer, "").unwrap();
            assert_eq!(id, StringOffset::ZERO);
            let foo_id = string_table.try_add(&mut buffer, "foo").unwrap();
            assert_eq!(1, foo_id.offset);
            let bar_id = string_table.try_add(&mut buffer, "bar").unwrap();
            assert_eq!(2, bar_id.offset);
            let php_id = string_table.try_add(&mut buffer, "<?php").unwrap();
            assert_eq!(3, php_id.offset);
            let index_id = string_table
                .try_add(&mut buffer, "/srv/project/public/index.php")
                .unwrap();
            assert_eq!(4, index_id.offset);
        }

        assert_eq!(string_table.len(), 5);

        let mut function_store = Store::<Function>::new();
        let id = function_store
            .add(
                &mut buffer,
                5,
                Function {
                    id: 10,
                    name: StringOffset { offset: 1 },
                    system_name: StringOffset::ZERO,
                    filename: StringOffset { offset: 4 },
                },
            )
            .unwrap();
        assert_eq!(10, id);

        // Tossing in another string to ensure out-of-order items.
        let main_id = string_table.try_add(&mut buffer, "main()").unwrap();
        assert_eq!(5, main_id.offset);

        let id = function_store
            .add(
                &mut buffer,
                5,
                Function {
                    id: 7,
                    name: StringOffset { offset: 3 },
                    system_name: StringOffset::ZERO,
                    filename: StringOffset { offset: 4 },
                },
            )
            .unwrap();
        assert_eq!(7, id);

        let data = &buffer[..];
        use prost::Message;
        let decoded = prost_impls::Profile::decode(data).unwrap();

        assert_eq!(decoded.string_table.len(), string_table.len());
        assert_eq!(decoded.string_table[0], "");
        assert_eq!(decoded.string_table[1], "foo");
        assert_eq!(decoded.string_table[2], "bar");
        assert_eq!(decoded.string_table[3], "<?php");
        assert_eq!(decoded.string_table[4], "/srv/project/public/index.php");
        assert_eq!(decoded.string_table[5], "main()");

        assert_eq!(decoded.functions.len(), 2);
        assert_eq!(decoded.functions[0].id, 10);
        assert_eq!(decoded.functions[0].name, 1);
        assert_eq!(decoded.functions[0].system_name, 0);
        assert_eq!(decoded.functions[0].filename, 4);

        assert_eq!(decoded.functions[1].id, 7);
        assert_eq!(decoded.functions[1].name, 3);
        assert_eq!(decoded.functions[1].system_name, 0);
        assert_eq!(decoded.functions[1].filename, 4);
    }
}
