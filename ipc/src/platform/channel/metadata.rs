use std::io;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use crate::handles::HandlesTransport;
use crate::platform::metadata::ChannelMetadata;
use crate::platform::PlatformHandle;

impl HandlesTransport for &mut ChannelMetadata {
    type Error = io::Error;

    fn move_handle<'h, T>(self, handle: PlatformHandle<T>) -> Result<(), Self::Error> {
        self.enqueue_for_sending(handle);

        Ok(())
    }

    fn provide_handle<T>(self, hint: &PlatformHandle<T>) -> Result<PlatformHandle<T>, Self::Error> {
        self.find_handle(hint).ok_or_else(|| {
            #[cfg(unix)]
            let handle = hint.as_raw_fd();
            #[cfg(windows)]
            let handle = hint.as_raw_handle();
            io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "can't provide expected handle for hint: {:?}",
                    handle
                ),
            )
        })
    }
}
