// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![allow(unused_variables)]
#![allow(dead_code)]
#![allow(clippy::todo)]

pub mod uploader;

use std::{
    ffi::c_void,
    time::{Duration, Instant},
};

use crate::profiles::datatypes::Profile;
use crossbeam_channel::{select, tick, Receiver, Sender};
use datadog_profiling::{api, internal};

#[repr(C)]
pub struct ManagedSample {
    data: *mut c_void,
    callback: extern "C" fn(*mut c_void, *mut Profile),
}

impl ManagedSample {
    /// Creates a new ManagedSample from a raw pointer and callback.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - The pointer is valid and points to memory that will remain valid for the lifetime of the
    ///   ManagedSample
    /// - The pointer is properly aligned
    /// - The memory is properly initialized
    #[no_mangle]
    pub unsafe extern "C" fn new(
        data: *mut c_void,
        callback: extern "C" fn(*mut c_void, *mut Profile),
    ) -> Self {
        Self { data, callback }
    }

    /// Returns the raw pointer to the underlying data.
    #[no_mangle]
    pub extern "C" fn as_ptr(&self) -> *mut c_void {
        self.data
    }

    #[allow(clippy::todo)]
    pub fn add(self, profile: &mut Profile) {
        (self.callback)(self.data, profile);
    }
}

pub struct ProfilerManager {
    samples_receiver: Receiver<ManagedSample>,
    samples_sender: Sender<ManagedSample>,
    cpu_ticker: Receiver<Instant>,
    upload_ticker: Receiver<Instant>,
    active: internal::Profile,
    standby: internal::Profile,
}

impl ProfilerManager {
    pub fn new(sample_types: &[api::ValueType], period: Option<api::Period>) -> Self {
        let (samples_sender, samples_receiver) = crossbeam_channel::bounded(10);
        let active = internal::Profile::new(sample_types, period);
        let standby = internal::Profile::new(sample_types, period);
        let cpu_ticker = tick(Duration::from_millis(100));
        let upload_ticker = tick(Duration::from_secs(1));
        Self {
            cpu_ticker,
            upload_ticker,
            samples_receiver,
            samples_sender,
            active,
            standby,
        }
    }

    #[allow(clippy::todo)]
    pub fn main(&mut self) -> anyhow::Result<()> {
        // This is just here to allow us to easily bail out.
        let done = tick(Duration::from_secs(5));
        loop {
            select! {
                recv(self.samples_receiver) -> sample => {
                    let mut ffi_profile = unsafe { Profile::from_pointer(&mut self.active as *mut _) };
                    sample?.add(&mut ffi_profile);
                },
                recv(self.cpu_ticker) -> msg => println!("{msg:?} call echion"),
                recv(self.upload_ticker) -> msg => println!("{msg:?} swap and upload the old one"),
                recv(done) -> msg => return Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::manager::ProfilerManager;

    #[test]
    fn test_the_thing() {
        let sample_types = [];
        let period = None;
        let mut profile_manager = ProfilerManager::new(&sample_types, period);
        println!("start");
        profile_manager.main().unwrap();
    }
}
