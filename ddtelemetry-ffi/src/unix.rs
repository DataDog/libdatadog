use std::{
    fs::File,
    os::unix::{net::UnixStream, prelude::FromRawFd},
};

use ddtelemetry::ipc::{platform::PlatformHandle, sidecar};

use crate::{try_c, MaybeError};

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_ph_file_from(file: *mut libc::FILE) -> Box<PlatformHandle<File>> {
    let handle = PlatformHandle::from_raw_fd(libc::fileno(file));

    Box::from(handle)
}

#[no_mangle]
pub extern "C" fn ddog_ph_file_clone(
    platform_handle: &PlatformHandle<File>,
) -> Box<PlatformHandle<File>> {
    Box::new(platform_handle.clone())
}

#[no_mangle]
pub extern "C" fn ddog_ph_file_drop(ph: Box<PlatformHandle<File>>) {
    drop(ph)
}

#[no_mangle]
pub extern "C" fn ddog_ph_unix_stream_drop(ph: Box<PlatformHandle<UnixStream>>) {
    drop(ph)
}

#[no_mangle]
pub extern "C" fn ddog_sidecar_connect(
    connection: &mut *mut PlatformHandle<UnixStream>,
) -> MaybeError {
    let stream = Box::new(try_c!(sidecar::start_or_connect_to_sidecar()).into());
    *connection = Box::into_raw(stream);

    MaybeError::None
}

#[cfg(test)]
mod test_c_sidecar {
    use super::*;
    use std::{
        ffi::CString,
        io::{Read, Write},
        os::unix::prelude::AsRawFd,
    };

    #[test]
    fn test_ddog_ph_file_handling() {
        let fname = CString::new(std::env::temp_dir().join("test_file").to_str().unwrap()).unwrap();
        let mode = CString::new("a+").unwrap();

        let file = unsafe { libc::fopen(fname.as_ptr(), mode.as_ptr()) };
        let file = unsafe { ddog_ph_file_from(file) };
        let fd = file.as_raw_fd();
        {
            let mut file = &*file.as_filelike_view().unwrap();
            writeln!(file, "test").unwrap();
        }
        ddog_ph_file_drop(file);

        let mut file = unsafe { File::from_raw_fd(fd) };
        writeln!(file, "test").unwrap_err(); // file is closed, so write returns an error
    }

    #[test]
    fn test_ddog_sidecar_connection() {
        let mut connection = std::ptr::null_mut();
        assert_eq!(ddog_sidecar_connect(&mut connection), MaybeError::None);
        let connection = unsafe { Box::from_raw(connection) };
        {
            let mut c = &*connection.as_socketlike_view().unwrap();
            writeln!(c, "test").unwrap();
            let mut buf = [0; 4];
            c.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"test");
        }
        ddog_ph_unix_stream_drop(connection);
    }
}
