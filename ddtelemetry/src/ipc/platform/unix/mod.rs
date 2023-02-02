// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

mod platform_handle;
pub use platform_handle::*;

mod channel;
pub use async_channel::*;
pub use channel::*;

pub mod locks;
pub mod sockets;

mod message;
pub use message::*;

#[cfg(test)]
mod tests {
    use io_lifetimes::OwnedFd;
    use pretty_assertions::assert_eq;
    use std::{
        collections::BTreeMap,
        fs::File,
        io::{self, Read, Seek, Write},
        os::unix::prelude::{AsRawFd, RawFd},
    };

    use crate::ipc::platform::{metadata::ChannelMetadata, unix::message::MAX_FDS};

    use super::PlatformHandle;

    fn assert_platform_handle_is_valid_file(
        handle: PlatformHandle<OwnedFd>,
    ) -> PlatformHandle<OwnedFd> {
        let mut file: File = unsafe { handle.to_any_type().into_instance().unwrap() };

        write!(file, "test_string").unwrap();
        file.rewind().unwrap();

        let mut data = String::new();
        file.read_to_string(&mut data).unwrap();
        assert_eq!("test_string", data);

        file.rewind().unwrap();
        PlatformHandle::from(file).to_untyped()
    }
    #[cfg(not(target_os = "macos"))]
    fn get_open_file_descriptors(
        pid: Option<libc::pid_t>,
    ) -> Result<BTreeMap<RawFd, String>, io::Error> {
        let proc = pid.map(|p| format!("{p}")).unwrap_or_else(|| "self".into());

        let fds_path = std::path::Path::new("/proc").join(proc).join("fd");
        let fds = std::fs::read_dir(fds_path)?
            .filter_map(|r| r.ok())
            .filter_map(|r| {
                let link = std::fs::read_link(r.path()).unwrap_or_default();
                let link = link.into_os_string().into_string().ok().unwrap_or_default();
                let fd = r.file_name().into_string().ok().unwrap_or_default();
                fd.parse().ok().map(|fd| (fd, link))
            })
            .collect();

        Ok(fds)
    }

    #[cfg(target_os = "macos")]
    fn get_open_file_descriptors(
        _: Option<libc::pid_t>,
    ) -> Result<BTreeMap<RawFd, String>, io::Error> {
        //TODO implement this check for macos
        Ok(BTreeMap::default())
    }

    fn assert_file_descriptors_unchanged(
        reference_meta: &BTreeMap<RawFd, String>,
        pid: Option<libc::pid_t>,
    ) {
        let current_meta = get_open_file_descriptors(pid).unwrap();

        assert_eq!(reference_meta, &current_meta);
    }

    #[test]
    #[ignore] // tests checks global FD state - so it needs to run in non-parallel mode
    fn test_channel_metadata_only_provides_valid_owned() {
        let reference = get_open_file_descriptors(None).unwrap();
        let mut meta = ChannelMetadata::default();

        // create real handles
        let files: Vec<File> = (0..)
            .map(|_| tempfile::tempfile().unwrap())
            .take(MAX_FDS * 2)
            .collect();
        let reference_open_files = get_open_file_descriptors(None).unwrap();

        // used for checking order of reenqueue behaviour
        let file_fds: Vec<RawFd> = files.iter().map(AsRawFd::as_raw_fd).collect();

        files
            .into_iter()
            .for_each(|f| meta.enqueue_for_sending(f.into()));

        let first_batch: Vec<PlatformHandle<OwnedFd>> = meta
            .drain_to_send()
            .into_iter()
            .map(assert_platform_handle_is_valid_file)
            .collect();

        assert_eq!(MAX_FDS, first_batch.len());

        meta.reenqueue_for_sending(first_batch);

        let mut handles = meta.drain_to_send();
        let second_batch = meta.drain_to_send();

        handles.extend(second_batch.into_iter());
        assert_eq!(MAX_FDS * 2, handles.len());
        assert_eq!(0, meta.drain_to_send().len());

        let final_ordered_fds_list: Vec<RawFd> = handles.iter().map(AsRawFd::as_raw_fd).collect();
        assert_eq!(file_fds, final_ordered_fds_list);

        assert_file_descriptors_unchanged(&reference_open_files, None);

        // test and dispose of all handles
        for handle in handles {
            assert_platform_handle_is_valid_file(handle);
        }

        assert_file_descriptors_unchanged(&reference, None);
    }
}
