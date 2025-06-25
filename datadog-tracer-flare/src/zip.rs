// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use walkdir::WalkDir;
use zip::{write::FileOptions, ZipWriter};

use crate::error::FlareError;

/// Creates a zip archive containing the specified files and directories in a temporary location.
///
/// This function takes a vector of file and directory paths, creates a zip archive containing
/// all the files, and returns the path to the created zip file. If a path is a directory,
/// all files within that directory (including subdirectories) are included in the archive.
///
/// # Arguments
///
/// * `files` - A vector of strings representing the paths of files and directories to include in
///   the zip archive.
/// * `temp_dir` - A temporary directory where the zip file will be created.
///
/// # Returns
///
/// * `Ok(PathBuf)` - The path to the created zip file if successful.
/// * `Err(FlareError)` - An error if any step of the process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The zip file cannot be created.
/// - Any file or directory cannot be read or added to the archive.
/// - The zip archive cannot be finalized.
/// - An invalid or non-existent path is provided.
fn zip_files(files: Vec<String>, temp_dir: &TempDir) -> Result<PathBuf, FlareError> {
    // Convert paths to PathBuf
    let paths: Vec<PathBuf> = files.into_iter().map(PathBuf::from).collect();

    // Create a temporary file for the zip
    let zip_path = temp_dir.path().join("flare.zip");
    let file = File::create(&zip_path)
        .map_err(|e| FlareError::ZipError(format!("Failed to create zip file: {}", e)))?;

    let mut zip = ZipWriter::new(file);
    let options = FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o755);

    // Iterate through all files and add them to the zip
    for path in paths {
        if path.is_dir() {
            // If it's a directory, iterate through all files
            for entry in WalkDir::new(&path) {
                let entry = entry.map_err(|e| {
                    FlareError::ZipError(format!("Failed to read directory entry: {}", e))
                })?;

                let file_path = entry.path();
                if file_path.is_file() {
                    // Get the directory name to create a folder in the zip
                    let dir_name = path.file_name().ok_or_else(|| {
                        FlareError::ZipError(format!("Invalid directory name for path: {:?}", path))
                    })?;

                    // Calculate the relative path from the base directory
                    let relative_path = file_path.strip_prefix(&path).map_err(|e| {
                        FlareError::ZipError(format!("Failed to calculate relative path: {}", e))
                    })?;

                    // Create the zip path with the directory name as prefix
                    let zip_path = PathBuf::from(dir_name).join(relative_path);

                    add_file_to_zip_with_path(&mut zip, file_path, &zip_path, &options)?;
                }
            }
        } else if path.is_file() {
            // If it's a file, add it directly
            add_file_to_zip(&mut zip, &path, &options)?;
        } else {
            return Err(FlareError::ZipError(format!(
                "Invalid or inexisting file: {}",
                path.to_string_lossy()
            )));
        }
    }

    // Finalize the zip
    zip.finish()
        .map_err(|e| FlareError::ZipError(format!("Failed to finalize zip file: {}", e)))?;

    Ok(zip_path)
}

/// Creates a zip archive containing the specified files and directories, obfuscates sensitive data,
/// and sends the flare to the agent.
///
/// # Arguments
///
/// * `files` - A vector of strings representing the paths of files and directories to include in
///   the zip archive.
///
/// # Returns
///
/// * `Ok(PathBuf)` - If the zip archive was created, obfuscated, and sent successfully.
/// * `Err(FlareError)` - An error if any step of the process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - Any problem happend while zipping the file.
/// - The obfuscation process fails.
/// - The zip file cannot be sent to the agent.
///
/// # Examples
///
/// ```
/// use datadog_tracer_flare::zip::zip_and_send;
///
/// let files = vec![
///     "/path/to/logs".to_string(),
///     "/path/to/config.txt".to_string(),
/// ];
/// match zip_and_send(files) {
///     Ok(_) => println!("Flare sent successfully"),
///     Err(e) => eprintln!("Failed to send flare: {}", e),
/// }
/// ```
pub fn zip_and_send(files: Vec<String>) -> Result<PathBuf, FlareError> {
    let temp_dir = match TempDir::new() {
        Ok(dir) => dir,
        Err(e) => return Err(FlareError::ZipError(e.to_string())),
    };
    let zip_path = zip_files(files, &temp_dir)?;

    // APMSP-2118 - TODO: Implement obfuscation of sensitive data
    // APMSP-1978 - TODO: Implement sending the zip file to the agent

    Ok(zip_path)
}

fn add_file_to_zip(
    zip: &mut ZipWriter<File>,
    file_path: &Path,
    options: &FileOptions<()>,
) -> Result<(), FlareError> {
    let file_name = file_path.file_name().ok_or_else(|| {
        FlareError::ZipError(format!("Invalid file name for path: {:?}", file_path))
    })?;

    let mut file = File::open(file_path)
        .map_err(|e| FlareError::ZipError(format!("Failed to open file {:?}: {}", file_path, e)))?;

    zip.start_file(file_name.to_string_lossy().as_ref(), *options)
        .map_err(|e| FlareError::ZipError(format!("Failed to add file to zip: {}", e)))?;

    io::copy(&mut file, zip)
        .map_err(|e| FlareError::ZipError(format!("Failed to write file to zip: {}", e)))?;

    Ok(())
}

fn add_file_to_zip_with_path(
    zip: &mut ZipWriter<File>,
    file_path: &Path,
    relative_path: &Path,
    options: &FileOptions<()>,
) -> Result<(), FlareError> {
    let mut file = File::open(file_path)
        .map_err(|e| FlareError::ZipError(format!("Failed to open file {:?}: {}", file_path, e)))?;

    // Use the relative path as the zip entry name to preserve folder structure
    zip.start_file(relative_path.to_string_lossy().as_ref(), *options)
        .map_err(|e| FlareError::ZipError(format!("Failed to add file to zip: {}", e)))?;

    io::copy(&mut file, zip)
        .map_err(|e| FlareError::ZipError(format!("Failed to write file to zip: {}", e)))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[allow(clippy::unwrap_used)]

    fn create_test_files(temp_dir: &TempDir) -> Vec<String> {
        // Create a simple file
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, "test content").unwrap();

        // Create a subdirectory with a file
        let sub_dir = temp_dir.path().join("subdir");
        std::fs::create_dir(&sub_dir).unwrap();
        let sub_file = sub_dir.join("subfile.txt");
        std::fs::write(&sub_file, "subdir content").unwrap();

        // Return the paths of files to zip
        vec![
            file_path.to_string_lossy().into_owned(),
            sub_dir.to_string_lossy().into_owned(),
        ]
    }

    #[test]
    fn test_zip_files() {
        // Create a temporary directory with test files
        let temp_dir = TempDir::new().unwrap();
        let files = create_test_files(&temp_dir);

        let zip_dir = TempDir::new().unwrap();
        let result = zip_files(files, &zip_dir);
        assert!(result.is_ok());
        let zip_path = result.unwrap();
        assert!(zip_path.exists());

        // Verify the zip content
        let zip_file = File::open(&zip_path).unwrap();
        let mut archive = zip::ZipArchive::new(zip_file).unwrap();

        assert!(archive.by_name("test.txt").is_ok());
        assert!(archive.by_name("subdir/subfile.txt").is_ok());

        let mut content = String::new();
        archive
            .by_name("test.txt")
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "test content");

        content.clear();
        archive
            .by_name("subdir/subfile.txt")
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "subdir content");
    }

    #[test]
    fn test_zip_files_with_invalid_path() {
        let temp_dir = TempDir::new().unwrap();
        let result = zip_files(vec!["/invalid/path".to_string()], &temp_dir);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), FlareError::ZipError(_)));
    }
}
