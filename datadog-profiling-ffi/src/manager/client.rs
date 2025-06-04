use std::{ffi::c_void, sync::atomic::AtomicBool, thread::JoinHandle};

use crossbeam_channel::{SendError, Sender, TryRecvError};
use datadog_profiling::internal;

use super::SampleChannels;
use super::SendSample;

pub struct ManagedProfilerClient {
    channels: SampleChannels,
    handle: JoinHandle<anyhow::Result<internal::Profile>>,
    shutdown_sender: Sender<()>,
    is_shutdown: AtomicBool,
}

impl ManagedProfilerClient {
    pub(crate) fn new(
        channels: SampleChannels,
        handle: JoinHandle<anyhow::Result<internal::Profile>>,
        shutdown_sender: Sender<()>,
    ) -> Self {
        Self {
            channels,
            handle,
            shutdown_sender,
            is_shutdown: AtomicBool::new(false),
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

    pub fn try_recv_recycled(&self) -> Result<*mut c_void, TryRecvError> {
        self.channels.try_recv_recycled()
    }

    pub fn shutdown(self) -> anyhow::Result<internal::Profile> {
        self.is_shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Todo: Should we report if there was an error sending the shutdown signal?
        let _ = self.shutdown_sender.send(());
        self.handle
            .join()
            .map_err(|e| anyhow::anyhow!("Failed to join handle: {:?}", e))?
    }
}
