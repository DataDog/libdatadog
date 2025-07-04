// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_trace_utils::send_with_retry::{send_with_retry, RetryStrategy};
use ddcommon::Endpoint;
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Seek},
    path::{Path, PathBuf},
    str::FromStr,
};
use tempfile::tempfile;
use walkdir::WalkDir;
use zip::{write::FileOptions, ZipWriter};

use crate::{error::FlareError, TracerFlare};

/// Adds a single file to the zip archive with the specified options and relative path
fn add_file_to_zip(
    zip: &mut ZipWriter<File>,
    file_path: &Path,
    relative_path: Option<&Path>,
    options: &FileOptions<()>,
) -> Result<(), FlareError> {
    let mut file = File::open(file_path)
        .map_err(|e| FlareError::ZipError(format!("Failed to open file {file_path:?}: {e}")))?;

    let path = match relative_path {
        Some(relative_path) => relative_path.as_os_str(),
        None => file_path.file_name().ok_or_else(|| {
            FlareError::ZipError(format!("Invalid file name for path: {file_path:?}"))
        })?,
    };

    zip.start_file(path.to_string_lossy().as_ref(), *options)
        .map_err(|e| FlareError::ZipError(format!("Failed to add file to zip: {e}")))?;

    io::copy(&mut file, zip)
        .map_err(|e| FlareError::ZipError(format!("Failed to write file to zip: {e}")))?;

    Ok(())
}

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
/// * `temp_file` - A temporary file where the zip will be created.
///
/// # Returns
///
/// * `Ok(File)` - The created zip file if successful.
/// * `Err(FlareError)` - An error if any step of the process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The zip file cannot be created.
/// - Any file or directory cannot be read or added to the archive.
/// - The zip archive cannot be finalized.
/// - An invalid or non-existent path is provided.
fn zip_files(files: Vec<String>) -> Result<File, FlareError> {
    let temp_file = match tempfile() {
        Ok(file) => file,
        Err(e) => return Err(FlareError::ZipError(e.to_string())),
    };

    // let file = temp_file
    //     .try_clone()
    //     .map_err(|e| FlareError::ZipError(format!("Failed to clone temp file: {e}")))?;

    let mut zip = ZipWriter::new(temp_file);
    let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let paths: Vec<PathBuf> = files.into_iter().map(PathBuf::from).collect();

    // Iterate through all files and add them to the zip
    for path in paths {
        if path.is_dir() {
            // If it's a directory, iterate through all files
            for entry in WalkDir::new(&path) {
                let entry = entry.map_err(|e| {
                    FlareError::ZipError(format!("Failed to read directory entry: {e}"))
                })?;

                let file_path = entry.path();
                if file_path.is_file() {
                    // Get the directory name to create a folder in the zip
                    let dir_name = path.file_name().ok_or_else(|| {
                        FlareError::ZipError(format!("Invalid directory name for path: {path:?}"))
                    })?;

                    // Calculate the relative path from the base directory
                    let relative_path = file_path.strip_prefix(&path).map_err(|e| {
                        FlareError::ZipError(format!("Failed to calculate relative path: {e}"))
                    })?;

                    // Create the zip path with the directory name as prefix
                    let zip_path = PathBuf::from(dir_name).join(relative_path);

                    add_file_to_zip(&mut zip, file_path, Some(&zip_path), &options)?;
                }
            }
        } else if path.is_file() {
            // If it's a file, add it directly
            add_file_to_zip(&mut zip, &path, None, &options)?;
        } else {
            return Err(FlareError::ZipError(format!(
                "Invalid or inexisting file: {}",
                path.to_string_lossy()
            )));
        }
    }

    // Finalize the zip
    let file = zip
        .finish()
        .map_err(|e| FlareError::ZipError(format!("Failed to finalize zip file: {e}")))?;

    Ok(file)
}

pub const BOUNDARY: &str = "83CAD6AA-8A24-462C-8B3D-FF9CC683B51B";

fn generate_payload(
    mut zip: File,
    language: &String,
    log_level: &String,
    case_id: &String,
    hostname: &String,
    user_handle: &String,
    uuid: &String,
) -> Result<Vec<u8>, FlareError> {
    let mut payload: Vec<u8> = Vec::new();

    // Create the multipart form data
    payload.extend_from_slice(format!("--{}\r\n", BOUNDARY).as_bytes());
    payload.extend_from_slice(b"Content-Disposition: form-data; name=\"source\"\r\n\r\n");
    payload.extend_from_slice(format!("tracer_{}", language).as_bytes());
    payload.extend_from_slice(b"\r\n");

    payload.extend_from_slice(format!("--{}\r\n", BOUNDARY).as_bytes());
    payload.extend_from_slice(b"Content-Disposition: form-data; name=\"case_id\"\r\n\r\n");
    payload.extend_from_slice(case_id.as_bytes());
    payload.extend_from_slice(b"\r\n");

    payload.extend_from_slice(format!("--{}\r\n", BOUNDARY).as_bytes());
    payload.extend_from_slice(b"Content-Disposition: form-data; name=\"hostname\"\r\n\r\n");
    payload.extend_from_slice(hostname.as_bytes());
    payload.extend_from_slice(b"\r\n");

    payload.extend_from_slice(format!("--{}\r\n", BOUNDARY).as_bytes());
    payload.extend_from_slice(b"Content-Disposition: form-data; name=\"email\"\r\n\r\n");
    payload.extend_from_slice(user_handle.as_bytes());
    payload.extend_from_slice(b"\r\n");

    payload.extend_from_slice(format!("--{}\r\n", BOUNDARY).as_bytes());
    payload.extend_from_slice(b"Content-Disposition: form-data; name=\"uuid\"\r\n\r\n");
    payload.extend_from_slice(uuid.as_bytes());
    payload.extend_from_slice(b"\r\n");

    // Add the description of the zip
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let filename = format!("tracer-{language}-{case_id}-{timestamp}-{log_level}.zip");
    payload.extend_from_slice(format!("--{}\r\n", BOUNDARY).as_bytes());
    payload.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"flare_file\"; filename=\"{}\"\r\n",
            filename
        )
        .as_bytes(),
    );
    payload.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");

    // Read the zip and add it
    let mut zip_content = Vec::new();
    zip.seek(std::io::SeekFrom::Start(0))
        .map_err(|e| FlareError::ZipError(format!("Failed to seek back to start: {e}")))?;
    zip.read_to_end(&mut zip_content)
        .map_err(|e| FlareError::ZipError(format!("Failed to read zip file: {e}")))?;

    println!("Zip : {:?}", zip_content);

    payload.extend_from_slice(&zip_content);
    payload.extend_from_slice(b"\r\n");

    // Final boundary
    payload.extend_from_slice(format!("--{}--\r\n", BOUNDARY).as_bytes());

    Ok(payload)
}

/// Sends a zip file to the agent via a POST request.
///
/// This function reads the entire zip file into memory, constructs an HTTP request
/// to the agent's flare endpoint, and sends it with retry logic. The agent URL is
/// automatically extended with the `/tracer_flare/v1` path.
///
/// # Arguments
///
/// * `zip` - A file handle to the zip archive to be sent
/// * `agent_url` - The base URL of the Datadog agent (e.g., "http://localhost:8126")
///
/// # Returns
///
/// * `Ok(())` - If the flare was successfully sent to the agent
/// * `Err(FlareError)` - If any step of the process fails (file reading, network, etc.)
///
/// # Errors
///
/// This function will return an error if:
/// - The zip file cannot be read into memory
/// - The agent URL is invalid
/// - The HTTP request fails after retries
/// - The agent returns a non-success HTTP status code
///
/// # Example
///
/// ```rust
/// use datadog_tracer_flare::zip::send;
/// use std::fs::File;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let zip_file = File::open("flare.zip")?;
///     let agent_url = "http://localhost:8126".to_string();
///
///     send(zip_file, agent_url).await?;
///     println!("Flare sent successfully!");
///     Ok(())
/// }
/// ```
async fn send(zip: File, tracer_flare: &mut TracerFlare) -> Result<(), FlareError> {
    let agent_task = match &tracer_flare.agent_task {
        None => {
            return Err(FlareError::SendError(
                "Trying to send the flare without AGENT_TASK received".to_string(),
            ))
        }
        Some(agent_task) => agent_task,
    };

    let payload = generate_payload(
        zip,
        &tracer_flare.language,
        &tracer_flare.log_level,
        &agent_task.args.case_id,
        &agent_task.args.hostname,
        &agent_task.args.user_handle,
        &agent_task.uuid,
    )?;

    let agent_url = tracer_flare.agent_url.clone() + "/tracer_flare/v1";
    let agent_url = match hyper::Uri::from_str(&agent_url) {
        Ok(uri) => uri,
        Err(_) => {
            return Err(FlareError::SendError(format!(
                "Invalid agent url: {agent_url}"
            )));
        }
    };

    let target = Endpoint {
        url: agent_url,
        ..Default::default()
    };

    let headers = HashMap::from([(
        hyper::header::CONTENT_TYPE.as_str(),
        format!("multipart/form-data; boundary={}", BOUNDARY),
    )]);

    let result = send_with_retry(&target, payload, &headers, &RetryStrategy::default(), None).await;
    match result {
        Ok((response, _attempts)) => {
            let status = response.status();
            if status.is_success() {
                // Should we return something specific ?
                Ok(())
            } else {
                // Maybe just put a warning log message ?
                Err(FlareError::SendError(format!(
                    "Agent returned non-success status for flare send: HTTP {}",
                    status
                )))
            }
        }
        Err(err) => Err(FlareError::SendError(err.to_string())),
    }
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
/// * `Ok(())` - If the zip archive was created, obfuscated, and sent successfully.
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
pub async fn zip_and_send(
    files: Vec<String>,
    tracer_flare: &mut TracerFlare,
) -> Result<(), FlareError> {
    let zip = zip_files(files)?;

    // APMSP-2118 - TODO: Implement obfuscation of sensitive data

    let response = send(zip, tracer_flare).await;

    tracer_flare.agent_task = None;
    tracer_flare.running = false;

    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::TempDir;

    fn create_test_files(temp_dir: &TempDir) -> Vec<String> {
        // Create a simple file
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, "test content").unwrap();

        // Create a directory with a file
        let dir = temp_dir.path().join("dir");
        std::fs::create_dir(&dir).unwrap();
        let sub_file = dir.join("subfile.txt");
        std::fs::write(&sub_file, "sub file content").unwrap();

        // Create a subdirectory with a file
        let sub_dir = dir.join("subdir");
        std::fs::create_dir(&sub_dir).unwrap();
        let sub_sub_file = sub_dir.join("subsubfile.txt");
        std::fs::write(&sub_sub_file, "sub sub file content").unwrap();

        // Return the paths of files to zip
        vec![
            file_path.to_string_lossy().into_owned(),
            dir.to_string_lossy().into_owned(),
        ]
    }

    #[test]
    fn test_zip_files() {
        // Create a temporary directory with test files
        let temp_dir = TempDir::new().unwrap();
        let files = create_test_files(&temp_dir);

        let result = zip_files(files);
        assert!(result.is_ok());

        // Verify the zip content
        let zip_file = result.unwrap();
        let mut archive = zip::ZipArchive::new(zip_file).unwrap();

        let dir_file = Path::new("dir").join("subfile.txt");
        let subdir_file = Path::new("dir").join("subdir").join("subsubfile.txt");

        assert!(archive.by_name("test.txt").is_ok());
        assert!(archive.by_name(dir_file.to_str().unwrap()).is_ok());
        assert!(archive.by_name(subdir_file.to_str().unwrap()).is_ok());

        let mut content = String::new();
        archive
            .by_name("test.txt")
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "test content");

        content.clear();
        archive
            .by_name(dir_file.to_str().unwrap())
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "sub file content");

        content.clear();
        archive
            .by_name(subdir_file.to_str().unwrap())
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "sub sub file content");
    }

    #[test]
    fn test_zip_files_with_invalid_path() {
        let result = zip_files(vec!["/invalid/path".to_string()]);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), FlareError::ZipError(_)));
    }
}
