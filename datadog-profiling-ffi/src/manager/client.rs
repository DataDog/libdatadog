use std::sync::Arc;
use std::{ffi::c_void, sync::atomic::AtomicBool, thread::JoinHandle};

use anyhow::{ensure, Result};
use crossbeam_channel::{SendError, Sender};
use datadog_profiling::internal;

use super::ClientSampleChannels;
use super::SendSample;

#[derive(Debug, Clone)]
pub struct ManagedProfilerClient {
    channels: ClientSampleChannels,
    is_shutdown: Arc<AtomicBool>,
}

pub struct ManagedProfilerController {
    handle: JoinHandle<Result<internal::Profile>>,
    shutdown_sender: Sender<()>,
    is_shutdown: Arc<AtomicBool>,
}

impl ManagedProfilerClient {
    pub fn new(channels: ClientSampleChannels, is_shutdown: Arc<AtomicBool>) -> Self {
        Self {
            channels,
            is_shutdown,
        }
    }

    /// # Safety
    /// The caller must ensure that:
    /// 1. The sample pointer is valid and points to a properly initialized sample
    /// 2. The caller transfers ownership of the sample to this function
    ///    - The sample is not being used by any other thread
    ///    - The sample must not be accessed by the caller after this call
    ///    - The manager will either free the sample or recycle it back
    /// 3. The sample will be properly cleaned up if it cannot be sent
    pub unsafe fn send_sample(&self, sample: *mut c_void) -> Result<(), SendError<SendSample>> {
        if self.is_shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(SendError(unsafe { SendSample::new(sample) }));
        }
        self.channels.send_sample(sample)
    }

    pub fn try_recv_recycled(
        &self,
    ) -> Result<*mut std::ffi::c_void, crossbeam_channel::TryRecvError> {
        if self.is_shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crossbeam_channel::TryRecvError::Disconnected);
        }
        self.channels.try_recv_recycled()
    }
}

impl ManagedProfilerController {
    pub fn new(
        handle: JoinHandle<Result<internal::Profile>>,
        shutdown_sender: Sender<()>,
        is_shutdown: Arc<AtomicBool>,
    ) -> Self {
        Self {
            handle,
            shutdown_sender,
            is_shutdown,
        }
    }

    pub fn shutdown(self) -> Result<internal::Profile> {
        ensure!(
            !self.is_shutdown.load(std::sync::atomic::Ordering::SeqCst),
            "Profiler manager is already shutdown"
        );
        self.shutdown_sender.send(())?;
        self.handle
            .join()
            .map_err(|e| anyhow::anyhow!("Failed to join manager thread: {:?}", e))?
    }
}
