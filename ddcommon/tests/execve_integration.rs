// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
// Integration test for PreparedExecve::exec

#![cfg(unix)]

use ddcommon::unix_utils::PreparedExecve;
use std::io::Read;
use std::os::unix::io::FromRawFd;

#[test]
fn test_prepared_execve_exec_echo_with_output() {
    use nix::sys::wait::{waitpid, WaitStatus};
    use nix::unistd::Pid;
    use std::os::unix::io::RawFd;

    // Create a pipe
    let mut pipe_fds = [0 as RawFd; 2];
    let res = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    assert_eq!(res, 0, "pipe failed");
    let (read_fd, write_fd) = (pipe_fds[0], pipe_fds[1]);

    // Fork the process
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");

    if pid == 0 {
        // Child process: redirect stdout to the pipe, then exec /bin/echo
        unsafe {
            libc::close(read_fd);
            libc::dup2(write_fd, libc::STDOUT_FILENO);
            libc::close(write_fd);
        }
        let args = vec!["echo".to_string(), "hello_integration_test".to_string()];
        let env = vec![];
        let execve =
            PreparedExecve::new("/bin/echo", &args, &env).expect("Failed to prepare execve");
        let _ = execve.exec();
        unsafe { libc::_exit(127) }
    } else {
        // Parent process: close write end, read from read end
        unsafe {
            libc::close(write_fd);
        }
        let mut output = Vec::new();
        let mut file = unsafe { std::fs::File::from_raw_fd(read_fd) };
        file.read_to_end(&mut output)
            .expect("Failed to read from pipe");
        // Wait for the child
        let child_pid = Pid::from_raw(pid);
        match waitpid(child_pid, None).expect("waitpid failed") {
            WaitStatus::Exited(_, status) => {
                assert_eq!(
                    status, 0,
                    "Child did not exit successfully: status={}",
                    status
                );
            }
            WaitStatus::Signaled(_, sig, _) => {
                panic!("Child terminated by signal: {:?}", sig);
            }
            other => {
                panic!("Unexpected wait status: {:?}", other);
            }
        }
        // Check the output
        let output_str = String::from_utf8_lossy(&output);
        // echo adds a newline
        assert_eq!(output_str, "hello_integration_test\n");
    }
}

#[test]
fn test_prepared_execve_exec_env_with_environment_variables() {
    use nix::sys::wait::{waitpid, WaitStatus};
    use nix::unistd::Pid;
    use std::os::unix::io::RawFd;

    // Create a pipe
    let mut pipe_fds = [0 as RawFd; 2];
    let res = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    assert_eq!(res, 0, "pipe failed");
    let (read_fd, write_fd) = (pipe_fds[0], pipe_fds[1]);

    // Fork the process
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");

    if pid == 0 {
        // Child process: redirect stdout to the pipe, then exec /usr/bin/env
        unsafe {
            libc::close(read_fd);
            libc::dup2(write_fd, libc::STDOUT_FILENO);
            libc::close(write_fd);
        }
        let args = vec!["env".to_string()];
        let env = vec![
            ("TEST_VAR1".to_string(), "value1".to_string()),
            ("TEST_VAR2".to_string(), "value with spaces".to_string()),
            (
                "TEST_VAR3".to_string(),
                "value_with_quotes=\"test\"".to_string(),
            ),
            ("EMOJI_VAR".to_string(), "🦀".to_string()),
        ];
        let execve =
            PreparedExecve::new("/usr/bin/env", &args, &env).expect("Failed to prepare execve");
        let _ = execve.exec();
        unsafe { libc::_exit(127) }
    } else {
        // Parent process: close write end, read from read end
        unsafe {
            libc::close(write_fd);
        }
        let mut output = Vec::new();
        let mut file = unsafe { std::fs::File::from_raw_fd(read_fd) };
        file.read_to_end(&mut output)
            .expect("Failed to read from pipe");
        // Wait for the child
        let child_pid = Pid::from_raw(pid);
        match waitpid(child_pid, None).expect("waitpid failed") {
            WaitStatus::Exited(_, status) => {
                assert_eq!(
                    status, 0,
                    "Child did not exit successfully: status={}",
                    status
                );
            }
            WaitStatus::Signaled(_, sig, _) => {
                panic!("Child terminated by signal: {:?}", sig);
            }
            other => {
                panic!("Unexpected wait status: {:?}", other);
            }
        }
        // Check the output contains our environment variables
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("TEST_VAR1=value1"));
        assert!(output_str.contains("TEST_VAR2=value with spaces"));
        assert!(output_str.contains("TEST_VAR3=value_with_quotes=\"test\""));
        assert!(output_str.contains("EMOJI_VAR=🦀"));
    }
}

#[cfg_attr(miri, ignore)] // miri doesn't support fork
#[test]
fn test_prepared_execve_exec_with_complex_arguments() {
    use nix::sys::wait::{waitpid, WaitStatus};
    use nix::unistd::Pid;
    use std::os::unix::io::RawFd;

    // Create a pipe
    let mut pipe_fds = [0 as RawFd; 2];
    let res = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    assert_eq!(res, 0, "pipe failed");
    let (read_fd, write_fd) = (pipe_fds[0], pipe_fds[1]);

    // Fork the process
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");

    if pid == 0 {
        // Child process: redirect stdout to the pipe, then exec /bin/echo with complex args
        unsafe {
            libc::close(read_fd);
            libc::dup2(write_fd, libc::STDOUT_FILENO);
            libc::close(write_fd);
        }
        let args = vec![
            "echo".to_string(),
            "arg1".to_string(),
            "arg with spaces".to_string(),
            "arg_with_quotes=\"test\"".to_string(),
            "arg_with_emoji=🦀".to_string(),
            "final_arg".to_string(),
        ];
        let env = vec![];
        let execve =
            PreparedExecve::new("/bin/echo", &args, &env).expect("Failed to prepare execve");
        let _ = execve.exec();
        unsafe { libc::_exit(127) }
    } else {
        // Parent process: close write end, read from read end
        unsafe {
            libc::close(write_fd);
        }
        let mut output = Vec::new();
        let mut file = unsafe { std::fs::File::from_raw_fd(read_fd) };
        file.read_to_end(&mut output)
            .expect("Failed to read from pipe");
        // Wait for the child
        let child_pid = Pid::from_raw(pid);
        match waitpid(child_pid, None).expect("waitpid failed") {
            WaitStatus::Exited(_, status) => {
                assert_eq!(
                    status, 0,
                    "Child did not exit successfully: status={}",
                    status
                );
            }
            WaitStatus::Signaled(_, sig, _) => {
                panic!("Child terminated by signal: {:?}", sig);
            }
            other => {
                panic!("Unexpected wait status: {:?}", other);
            }
        }
        // Check the output contains all arguments
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("arg1"));
        assert!(output_str.contains("arg with spaces"));
        assert!(output_str.contains("arg_with_quotes=\"test\""));
        assert!(output_str.contains("arg_with_emoji=🦀"));
        assert!(output_str.contains("final_arg"));
    }
}

#[test]
fn test_prepared_execve_exec_nonexistent_binary() {
    use nix::sys::wait::{waitpid, WaitStatus};
    use nix::unistd::Pid;

    // Fork the process
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");

    if pid == 0 {
        // Child process: try to exec a non-existent binary
        let args = vec!["nonexistent".to_string()];
        let env = vec![];
        let execve = PreparedExecve::new("/nonexistent/binary", &args, &env)
            .expect("Failed to prepare execve");
        let _ = execve.exec();
        // If exec fails, exit with error code 127 (command not found)
        unsafe { libc::_exit(127) }
    } else {
        // Parent process: wait for the child
        let child_pid = Pid::from_raw(pid);
        match waitpid(child_pid, None).expect("waitpid failed") {
            WaitStatus::Exited(_, status) => {
                // Should exit with 127 (command not found)
                assert_eq!(
                    status, 127,
                    "Child should have exited with 127, got {}",
                    status
                );
            }
            WaitStatus::Signaled(_, sig, _) => {
                panic!("Child terminated by signal: {:?}", sig);
            }
            other => {
                panic!("Unexpected wait status: {:?}", other);
            }
        }
    }
}
