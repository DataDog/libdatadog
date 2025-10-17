// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::datatypes::{
    AttributeSet, EndpointTracker, LinkSet, Location2, LocationSet, StackSet,
};
use crate::profiles::ProfileError;
use std::time::SystemTime;

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

    // Start/end timestamps for the interval shared by profiles using this scratchpad.
    start_time: Option<SystemTime>,
    end_time: Option<SystemTime>,
}

impl ScratchPad {
    pub fn try_new() -> Result<ScratchPad, ProfileError> {
        let scratchpad = ScratchPad {
            attributes: AttributeSet::try_new()?,
            links: LinkSet::try_new()?,
            stacks: StackSet::try_new()?,
            locations: LocationSet::try_new()?,
            endpoint_tracker: EndpointTracker::try_new()?,
            start_time: None,
            end_time: None,
        };

        scratchpad.locations.try_insert(Location2::default())?;

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

    pub fn set_start_time(&mut self, start: SystemTime) {
        self.start_time = Some(start);
    }

    pub fn set_end_time(&mut self, end: SystemTime) -> Result<(), ProfileError> {
        if let Some(start) = self.start_time {
            if end < start {
                return Err(ProfileError::other("end time earlier than start time"));
            }
        }
        self.end_time = Some(end);
        Ok(())
    }

    pub fn interval(&self) -> Option<(SystemTime, SystemTime)> {
        match (self.start_time, self.end_time) {
            (Some(s), Some(e)) => Some((s, e)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::marker::PhantomData;

    fn is_send<T: Send>(_: PhantomData<T>) -> bool {
        true
    }
    fn is_sync<T: Sync>(_: PhantomData<T>) -> bool {
        true
    }

    #[test]
    fn test_send_and_sync() {
        assert!(is_send::<ScratchPad>(PhantomData));
        assert!(is_sync::<ScratchPad>(PhantomData));
    }
}
