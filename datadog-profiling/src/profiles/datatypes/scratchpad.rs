// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::datatypes::{
    AttributeSet, EndpointTracker, LinkSet, Location, LocationSet, StackSet,
};
use crate::profiles::ProfileError;

/// The profile scratchpad is for data which is scoped to the profiling
/// interval, which commonly 60 seconds. If this data was stored longer than
/// the interval, then process memory would likely balloon. This is shared by
/// all profiles associated to that interval.
pub struct ScratchPad {
    attributes: AttributeSet,
    links: LinkSet,
    stacks: StackSet,

    // The locations could possibly be stored in the ProfilesDictionary, but
    // given that each function has 1 or more lines in it, it seems prudent
    // for now to store this in the scratchpad.
    locations: LocationSet,

    // A mapping from local root span id to trace endpoints.
    endpoint_tracker: EndpointTracker,
}

impl ScratchPad {
    pub fn try_new() -> Result<ScratchPad, ProfileError> {
        let scratchpad = ScratchPad {
            attributes: AttributeSet::try_new()?,
            links: LinkSet::try_new()?,
            stacks: StackSet::try_new()?,
            locations: LocationSet::try_new()?,
            endpoint_tracker: EndpointTracker::try_new()?,
        };

        scratchpad.locations.try_insert(Location::default())?;

        Ok(scratchpad)
    }

    pub fn attributes(&self) -> &AttributeSet {
        &self.attributes
    }

    pub fn links(&self) -> &LinkSet {
        &self.links
    }

    pub fn locations(&self) -> &LocationSet {
        &self.locations
    }

    pub fn stacks(&self) -> &StackSet {
        &self.stacks
    }

    pub fn endpoint_tracker(&self) -> &EndpointTracker {
        &self.endpoint_tracker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::marker::PhantomData;

    fn is_send<T: Send>(_: PhantomData<T>) -> bool { true }
    fn is_sync<T: Sync>(_: PhantomData<T>) -> bool { true }

    #[test]
    fn test_send_and_sync() {
        assert!(is_send::<ScratchPad>(PhantomData));
        assert!(is_sync::<ScratchPad>(PhantomData));
    }
}
