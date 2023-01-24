// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use crate::profile2::{Function, Id, Location, Mapping};
use std::fmt::Debug;
use std::hash::Hash;

/// The Storable trait must be implemented for things that can be stored in
/// ProfTables.
pub trait Storable: Clone + Debug + Default + Eq + Hash + PartialEq {
    fn set_id(&mut self, id: Id);
    fn get_id(&self) -> Id;
}

impl Storable for Function {
    fn set_id(&mut self, id: Id) {
        self.id = id.into()
    }

    fn get_id(&self) -> Id {
        self.id.into()
    }
}

impl Storable for Location {
    fn set_id(&mut self, id: Id) {
        self.id = id.into()
    }

    fn get_id(&self) -> Id {
        self.id.into()
    }
}

impl Storable for Mapping {
    fn set_id(&mut self, id: Id) {
        self.id = id.into()
    }

    fn get_id(&self) -> Id {
        self.id.into()
    }
}
