use std::ffi::c_void;

use crossbeam_channel::{Receiver, SendError, Sender, TryRecvError};

// TODO: this owns the memory.  It should probably be a full wrapper, with a destructor.
#[repr(transparent)]
pub struct SendSample(*mut c_void);

// SAFETY: This type is used to transfer ownership of a sample between threads via channels.
// The sample is only accessed by one thread at a time, and ownership is transferred along
// with the SendSample wrapper. The sample is either processed by the manager thread or
// recycled back to the original thread.
unsafe impl Send for SendSample {}

impl SendSample {
    /// # Safety
    /// The caller must ensure that:
    /// 1. The sample pointer is valid and points to a properly initialized sample
    /// 2. The sample is not being used by any other thread
    /// 3. The caller transfers ownership of the sample to this function
    pub unsafe fn new(ptr: *mut c_void) -> Self {
        Self(ptr)
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.0
    }
}

pub struct ClientSampleChannels {
    samples_sender: Sender<SendSample>,
    recycled_samples_receiver: Receiver<SendSample>,
}

pub struct ManagerSampleChannels {
    pub samples_receiver: Receiver<SendSample>,
    pub recycled_samples_sender: Sender<SendSample>,
    pub recycled_samples_receiver: Receiver<SendSample>,
}

impl ClientSampleChannels {
    pub fn new(channel_depth: usize) -> (Self, ManagerSampleChannels) {
        let (samples_sender, samples_receiver) = crossbeam_channel::bounded(channel_depth);
        let (recycled_samples_sender, recycled_samples_receiver) =
            crossbeam_channel::bounded(channel_depth);
        (
            Self {
                samples_sender,
                recycled_samples_receiver: recycled_samples_receiver.clone(),
            },
            ManagerSampleChannels {
                samples_receiver,
                recycled_samples_sender,
                recycled_samples_receiver,
            },
        )
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
        self.samples_sender.send(SendSample::new(sample))
    }

    pub fn try_recv_recycled(&self) -> Result<*mut c_void, TryRecvError> {
        self.recycled_samples_receiver
            .try_recv()
            .map(|sample| sample.as_ptr())
    }
}
