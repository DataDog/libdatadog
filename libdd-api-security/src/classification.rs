// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{Scanner, schema::Node};

// The structural visitor carries no scanner or key state. Keeping this a
// separate monomorphization also lets bindings omit unused regex machinery.
pub trait Classification<'a>: Copy {
    type WithKey<'key>: Classification<'a>;
    fn with_key(self, key: &str) -> Self::WithKey<'_>;
    fn string(self, value: &str) -> Node<'a>;
}

#[derive(Clone, Copy)]
pub struct Structural;

impl<'a> Classification<'a> for Structural {
    type WithKey<'key> = Self;
    fn with_key(self, _: &str) -> Self {
        self
    }
    fn string(self, _: &str) -> Node<'a> {
        Node::Scalar(8)
    }
}

#[derive(Clone, Copy)]
pub struct Classifiers<'a, 'key> {
    pub scanners: &'a [Scanner],
    pub key: &'key str,
}

impl<'a> Classification<'a> for Classifiers<'a, '_> {
    type WithKey<'key> = Classifiers<'a, 'key>;
    fn with_key(self, key: &str) -> Self::WithKey<'_> {
        Classifiers {
            scanners: self.scanners,
            key,
        }
    }
    fn string(self, value: &str) -> Node<'a> {
        Node::string(value, self.key, self.scanners)
    }
}
