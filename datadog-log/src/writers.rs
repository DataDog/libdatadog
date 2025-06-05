// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::logger::FileConfig;
use crate::logger::StdTarget;
use chrono;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::{fs, io};
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::fmt::MakeWriter;

/// A custom file appender that handles optional size-based rotation
/// tokio doesn't support file rotation, so we need to implement it ourselves.
/// https://github.com/tokio-rs/tracing/pull/2497
struct CustomFileAppender {
    path: PathBuf,
    current_size: u64,
    max_size: u64,
    max_files: u64,
    current_file: fs::File,
}

impl CustomFileAppender {
    fn new(config: &FileConfig) -> io::Result<Self> {
        let path = Path::new(&config.path).to_path_buf();
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        let current_size = file.metadata()?.len();

        Ok(Self {
            path,
            current_size,
            max_size: config.max_size_bytes,
            max_files: config.max_files,
            current_file: file,
        })
    }

    fn get_timestamp_string() -> String {
        chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string()
    }

    /// Build the rotated file path.
    /// The rotated files names are appended with their rotated at timestamp.
    /// The file extension is preserved.
    /// If the file has no extension, the timestamp is appended without a dot.
    fn build_rotated_path(&self, timestamp: &str) -> PathBuf {
        match (self.path.file_stem(), self.path.extension()) {
            (Some(stem), Some(ext)) => {
                let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
                parent.join(format!(
                    "{}_{}.{}",
                    stem.to_string_lossy(),
                    timestamp,
                    ext.to_string_lossy()
                ))
            }
            (Some(stem), None) => {
                let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
                parent.join(format!("{}_{}", stem.to_string_lossy(), timestamp))
            }
            (None, _) => PathBuf::from(format!("{}_{}", self.path.display(), timestamp)),
        }
    }

    /// Rotate the file if it exceeds the maximum size.
    /// If the file exceeds the maximum size, it will be renamed to a new file with a timestamp
    /// and the current file will be closed.
    /// If the maximum number of files is exceeded, the oldest rotated files will be deleted.
    /// The rotated files names are appended with their rotated at timestamp.
    fn rotate_if_needed(&mut self) -> io::Result<()> {
        if self.max_size > 0 && self.current_size >= self.max_size {
            self.current_file.flush()?;

            let timestamp = Self::get_timestamp_string();
            let new_path = self.build_rotated_path(&timestamp);

            fs::rename(&self.path, new_path)?;

            self.current_file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;

            self.current_size = 0;

            if self.max_files > 0 {
                self.cleanup_old_files(self.max_files)?;
            }
        }
        Ok(())
    }

    /// Cleanup old files when the maximum number of files is exceeded.
    /// The files are sorted by timestamp (newest first) to ensure we keep the most recent files
    /// and delete the oldest ones when cleanup is needed.
    /// The current file is never deleted.
    fn cleanup_old_files(&self, max_files: u64) -> io::Result<()> {
        if max_files == 0 {
            return Ok(());
        }

        let parent_dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        let (base_name, extension) = match (self.path.file_stem(), self.path.extension()) {
            (Some(stem), ext) => (
                stem.to_string_lossy().to_string(),
                ext.map(|e| e.to_string_lossy().to_string()),
            ),
            _ => return Ok(()),
        };

        let mut rotated_files: Vec<_> = fs::read_dir(parent_dir)?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let file_name = entry.file_name().to_string_lossy().to_string();

                let expected_prefix = format!("{}_", base_name);
                if file_name.starts_with(&expected_prefix) {
                    match &extension {
                        Some(ext) => {
                            if file_name.ends_with(&format!(".{}", ext)) {
                                let timestamp_part = &file_name
                                    [expected_prefix.len()..file_name.len() - ext.len() - 1];
                                Some((entry.path(), timestamp_part.to_string()))
                            } else {
                                None
                            }
                        }
                        None => {
                            let timestamp_part = &file_name[expected_prefix.len()..];
                            Some((entry.path(), timestamp_part.to_string()))
                        }
                    }
                } else {
                    None
                }
            })
            .collect();

        // Sort by timestamp (newest first) - this ensures we keep the most recent files
        // and delete the oldest ones when cleanup is needed
        rotated_files.sort_by(|(_, timestamp_a), (_, timestamp_b)| timestamp_b.cmp(timestamp_a));

        let max_rotated_files = max_files.saturating_sub(1);

        if rotated_files.len() > max_rotated_files as usize {
            let mut cleanup_errors = Vec::new();

            for (file_path, _) in rotated_files.iter().skip(max_rotated_files as usize) {
                if let Err(e) = fs::remove_file(file_path) {
                    cleanup_errors.push(format!("{}: {}", file_path.display(), e));
                }
            }

            if !cleanup_errors.is_empty() {
                return Err(io::Error::other(format!(
                    "Failed to remove old log files: {}",
                    cleanup_errors.join(", ")
                )));
            }
        }

        Ok(())
    }
}

impl Write for CustomFileAppender {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.rotate_if_needed()?;
        let written = self.current_file.write(buf)?;
        self.current_size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.current_file.flush()
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.rotate_if_needed()?;
        self.current_file.write_all(buf)?;
        self.current_size += buf.len() as u64;
        Ok(())
    }
}

/// A non-blocking writer that writes log output to a file.
///
/// Uses a background thread to handle writes asynchronously, which improves
/// performance by not blocking the logging thread. The background thread is
/// managed by the internal `WorkerGuard`.
pub struct FileWriter {
    non_blocking: NonBlocking,
    /// The WorkerGuard is crucial for the non-blocking writer's functionality.
    ///
    /// The guard represents ownership of the background worker thread that processes
    /// writes asynchronously. When the guard is dropped, it ensures:
    /// 1. All pending writes are flushed
    /// 2. The worker thread is properly shut down
    /// 3. No writes are lost
    ///
    /// If we don't keep the guard alive for the entire lifetime of the writer:
    /// - The worker thread might be shut down prematurely
    /// - Pending writes could be lost
    /// - The non-blocking writer would stop functioning
    ///
    /// That's why we store it in the struct and name it with a leading underscore
    /// to indicate it's intentionally unused but must be kept alive.
    _guard: WorkerGuard,
}

impl FileWriter {
    /// Creates a new file writer that writes to the specified path.
    ///
    /// If the parent directory doesn't exist, it will be created.
    /// The file will be opened in append mode.
    /// If size_bytes is specified in the config, the file will be rotated when it reaches that
    /// size.
    pub fn new(config: &FileConfig) -> io::Result<Self> {
        let path = Path::new(&config.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file_appender = CustomFileAppender::new(config)?;
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        Ok(Self {
            non_blocking,
            _guard: guard,
        })
    }
}

impl Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.non_blocking.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.non_blocking.flush()
    }
}

impl<'a> MakeWriter<'a> for FileWriter {
    type Writer = NonBlocking;

    fn make_writer(&'a self) -> Self::Writer {
        self.non_blocking.clone()
    }
}

/// A writer that writes log output to standard output or standard error.
pub struct StdWriter {
    target: StdTarget,
}

impl StdWriter {
    /// Creates a new writer that writes to the specified standard stream.
    pub fn new(target: StdTarget) -> Self {
        Self { target }
    }
}

impl Write for StdWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.target {
            StdTarget::Out => io::stdout().write(buf),
            StdTarget::Err => io::stderr().write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.target {
            StdTarget::Out => io::stdout().flush(),
            StdTarget::Err => io::stderr().flush(),
        }
    }
}

impl<'a> MakeWriter<'a> for StdWriter {
    type Writer = StdWriter;

    fn make_writer(&'a self) -> Self::Writer {
        StdWriter::new(self.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_file_writer_basic_functionality() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.log");
        let config = FileConfig {
            path: file_path.to_str().unwrap().to_string(),
            max_size_bytes: 0,
            max_files: 0,
        };

        let mut writer = FileWriter::new(&config).unwrap();

        let test_data = b"Hello, World!\n";
        let written = writer.write(test_data).unwrap();
        assert_eq!(written, test_data.len());

        writer.flush().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(100));

        assert!(file_path.exists());
        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("Hello, World!"));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_file_writer_creates_directories() {
        let temp_dir = TempDir::new().unwrap();
        let nested_path = temp_dir
            .path()
            .join("subdir")
            .join("nested")
            .join("test.log");
        let config = FileConfig {
            path: nested_path.to_str().unwrap().to_string(),
            max_size_bytes: 0,
            max_files: 0,
        };

        let writer = FileWriter::new(&config);
        assert!(writer.is_ok());

        assert!(nested_path.parent().unwrap().exists());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_basic_rotation() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("rotate.log");
        let config = FileConfig {
            path: file_path.to_str().unwrap().to_string(),
            max_size_bytes: 5,
            max_files: 0,
        };

        let mut appender = CustomFileAppender::new(&config).unwrap();

        appender.write_all(b"123456").unwrap(); // 6 bytes > 5 byte limit
        appender.write_all(b"X").unwrap(); // Triggers rotation

        let parent_dir = file_path.parent().unwrap();
        let file_count = fs::read_dir(parent_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                name.starts_with("rotate") && name.ends_with(".log")
            })
            .count();

        assert_eq!(file_count, 2);
        assert!(file_path.exists());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_max_files_cleanup() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("cleanup.log");
        let config = FileConfig {
            path: file_path.to_str().unwrap().to_string(),
            max_size_bytes: 5,
            max_files: 2,
        };

        let mut appender = CustomFileAppender::new(&config).unwrap();

        for _ in 0..3 {
            appender.write_all(b"123456").unwrap(); // Exceed limit
            appender.write_all(b"X").unwrap(); // Trigger rotation
            std::thread::sleep(std::time::Duration::from_millis(10)); // Ensure different timestamps
        }

        let parent_dir = file_path.parent().unwrap();
        let file_count = fs::read_dir(parent_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                name.starts_with("cleanup") && name.ends_with(".log")
            })
            .count();

        assert_eq!(file_count, 2);
        assert!(file_path.exists());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_max_files_one_keeps_only_current() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("single.log");
        let config = FileConfig {
            path: file_path.to_str().unwrap().to_string(),
            max_size_bytes: 5,
            max_files: 1,
        };

        let mut appender = CustomFileAppender::new(&config).unwrap();

        for _ in 0..2 {
            appender.write_all(b"123456").unwrap(); // Exceed limit
            appender.write_all(b"X").unwrap(); // Trigger rotation
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let parent_dir = file_path.parent().unwrap();
        let file_count = fs::read_dir(parent_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                name.starts_with("single") && name.ends_with(".log")
            })
            .count();

        assert_eq!(file_count, 1);
        assert!(file_path.exists());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_no_rotation_when_size_not_specified() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("no_rotation.log");
        let config = FileConfig {
            path: file_path.to_str().unwrap().to_string(),
            max_size_bytes: 0,
            max_files: 0,
        };

        let mut appender = CustomFileAppender::new(&config).unwrap();

        for _ in 0..10 {
            appender.write_all(&vec![b'X'; 1000]).unwrap();
        }

        let parent_dir = file_path.parent().unwrap();
        let file_count = fs::read_dir(parent_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                name.starts_with("no_rotation") && name.ends_with(".log")
            })
            .count();

        assert_eq!(file_count, 1);
        assert!(file_path.exists());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_invalid_path_handling() {
        let config = FileConfig {
            path: String::new(),
            max_size_bytes: 0,
            max_files: 0,
        };
        let result = FileWriter::new(&config);
        assert!(result.is_err());
    }
}
