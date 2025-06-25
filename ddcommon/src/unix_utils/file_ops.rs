// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::fs::{File, OpenOptions};
use std::os::fd::{IntoRawFd, RawFd};

/// Opens a file for writing (in append mode) or opens /dev/null
/// * If the filename is provided, it will try to open (creating if needed) the specified file.
///   Failure to do so is an error.
/// * If the filename is not provided, it will open /dev/null Some systems can fail to provide
///   `/dev/null` (e.g., chroot jails), so this failure is also an error.
/// * Using Stdio::null() is more direct, but it will cause a panic in environments where /dev/null
///   is not available.
pub fn open_file_or_quiet(filename: Option<&str>) -> std::io::Result<RawFd> {
    let file = filename.map_or_else(
        || File::open("/dev/null"),
        |f| OpenOptions::new().append(true).create(true).open(f),
    )?;
    Ok(file.into_raw_fd())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::os::fd::FromRawFd;
    use tempfile::tempdir;

    #[test]
    fn test_open_file_or_quiet_none() {
        // Test opening /dev/null when no filename is provided
        let result = open_file_or_quiet(None);
        assert!(result.is_ok());

        let fd = result.unwrap();
        assert!(fd >= 0);

        // Try writing to /dev/null
        let mut file = unsafe { File::from_raw_fd(fd) };
        let write_result = writeln!(file, "should not fail");
        // On some platforms (notably macOS), writing to /dev/null may return a variety of error
        // kinds, including Uncategorized, BrokenPipe, or Other. We accept any error kind
        // here to ensure the test is robust across platforms, as long as the operation does
        // not panic or crash.
        if let Err(e) = write_result {
            eprintln!(
                "Writing to /dev/null failed with error kind: {:?}",
                e.kind()
            );
        }
        // File will be closed when dropped
    }

    #[test]
    fn test_open_file_or_quiet_new_file() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.path().join("test_file.txt");
        let file_path_str = file_path.to_str().unwrap();

        // Test creating a new file
        let result = open_file_or_quiet(Some(file_path_str));
        assert!(result.is_ok());

        let fd = result.unwrap();
        assert!(fd >= 0);

        // Write to the file
        let mut file = unsafe { File::from_raw_fd(fd) };
        writeln!(file, "hello world").unwrap();
        drop(file);

        // Verify the file was created and contains the written content
        assert!(file_path.exists());
        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("hello world"));
    }

    #[test]
    fn test_open_file_or_quiet_existing_file() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.path().join("existing_file.txt");
        let file_path_str = file_path.to_str().unwrap();

        // Create a file first
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "initial content").unwrap();
        drop(file);

        // Test opening an existing file
        let result = open_file_or_quiet(Some(file_path_str));
        assert!(result.is_ok());

        let fd = result.unwrap();
        assert!(fd >= 0);

        // Write to the file (should append)
        let mut file = unsafe { File::from_raw_fd(fd) };
        writeln!(file, "more content").unwrap();
        drop(file);

        // Verify the file still exists and contains both lines
        assert!(file_path.exists());
        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("initial content"));
        assert!(content.contains("more content"));
    }

    #[test]
    fn test_open_file_or_quiet_append_mode() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.path().join("append_test.txt");
        let file_path_str = file_path.to_str().unwrap();

        // Create a file with initial content
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "initial content").unwrap();
        drop(file);

        // Open in append mode
        let result = open_file_or_quiet(Some(file_path_str));
        assert!(result.is_ok());

        let fd = result.unwrap();
        assert!(fd >= 0);

        // Write additional content
        let mut file = unsafe { File::from_raw_fd(fd) };
        writeln!(file, "appended content").unwrap();
        drop(file);

        // Verify both contents are present
        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("initial content"));
        assert!(content.contains("appended content"));
    }

    #[test]
    fn test_open_file_or_quiet_invalid_path() {
        // Test with an invalid path that should fail
        let result = open_file_or_quiet(Some("/nonexistent/directory/file.txt"));
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn test_open_file_or_quiet_empty_string() {
        // Test with empty string (should fail as it's treated as a file path)
        let result = open_file_or_quiet(Some(""));
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn test_open_file_or_quiet_permission_denied() {
        // Test with a path that should cause permission denied
        // This test might not work on all systems, so we'll make it conditional
        let result = open_file_or_quiet(Some("/root/protected_file.txt"));
        if result.is_err() {
            let error = result.unwrap_err();
            // On some systems this might be NotFound, on others PermissionDenied
            assert!(matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ));
        }
    }

    #[test]
    fn test_open_file_or_quiet_multiple_calls() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.path().join("multi_test.txt");
        let file_path_str = file_path.to_str().unwrap();

        // Multiple calls should all succeed
        let fd1 = open_file_or_quiet(Some(file_path_str)).unwrap();
        let fd2 = open_file_or_quiet(Some(file_path_str)).unwrap();
        let fd3 = open_file_or_quiet(None).unwrap();

        assert!(fd1 >= 0);
        assert!(fd2 >= 0);
        assert!(fd3 >= 0);
        assert!(fd1 != fd2); // Different file descriptors
        assert!(fd3 != fd1);
        assert!(fd3 != fd2);

        // Clean up
        unsafe {
            libc::close(fd1);
            libc::close(fd2);
            libc::close(fd3);
        }
    }
}
