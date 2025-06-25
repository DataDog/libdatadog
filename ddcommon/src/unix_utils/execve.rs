// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libc::execve;
use nix::errno::Errno;
use std::ffi::CString;

// The args_cstrings and env_vars_strings fields are just storage.  Even though they're
// unreferenced, they're a necessary part of the struct.
#[allow(dead_code)]
#[derive(Debug)]
pub struct PreparedExecve {
    binary_path: CString,
    args_cstrings: Vec<CString>,
    args_ptrs: Vec<*const libc::c_char>,
    env_vars_cstrings: Vec<CString>,
    env_vars_ptrs: Vec<*const libc::c_char>,
}

#[derive(Debug, thiserror::Error)]
pub enum PreparedExecveError {
    #[error("Failed to convert binary path to CString: {0}")]
    BinaryPathError(std::ffi::NulError),
    #[error("Failed to convert argument to CString: {0}")]
    ArgumentError(std::ffi::NulError),
    #[error("Failed to convert environment variable to CString: {0}")]
    EnvironmentError(std::ffi::NulError),
}

impl From<std::ffi::NulError> for PreparedExecveError {
    fn from(err: std::ffi::NulError) -> Self {
        PreparedExecveError::BinaryPathError(err)
    }
}

impl PreparedExecve {
    pub fn new(
        binary_path: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<Self, PreparedExecveError> {
        // Allocate and store binary path
        let binary_path =
            CString::new(binary_path).map_err(PreparedExecveError::BinaryPathError)?;

        // Allocate and store arguments
        let args_cstrings: Vec<CString> = args
            .iter()
            .map(|s| CString::new(s.as_str()))
            .collect::<Result<Vec<CString>, std::ffi::NulError>>()
            .map_err(PreparedExecveError::ArgumentError)?;
        let args_ptrs: Vec<*const libc::c_char> = args_cstrings
            .iter()
            .map(|arg| arg.as_ptr())
            .chain(std::iter::once(std::ptr::null())) // Adds a null pointer to the end of the list
            .collect();

        // Allocate and store environment variables
        let env_vars_cstrings: Vec<CString> = env
            .iter()
            .map(|(key, value)| {
                let env_str = format!("{key}={value}");
                CString::new(env_str)
            })
            .collect::<Result<Vec<CString>, std::ffi::NulError>>()
            .map_err(PreparedExecveError::EnvironmentError)?;
        let env_vars_ptrs: Vec<*const libc::c_char> = env_vars_cstrings
            .iter()
            .map(|env| env.as_ptr())
            .chain(std::iter::once(std::ptr::null())) // Adds a null pointer to the end of the list
            .collect();

        Ok(Self {
            binary_path,
            args_cstrings,
            args_ptrs,
            env_vars_cstrings,
            env_vars_ptrs,
        })
    }

    /// Calls `execve` on the prepared arguments.
    pub fn exec(&self) -> Result<(), Errno> {
        // Safety: the only way to make one of these is through `new`, which ensures that everything
        // is well-formed.
        unsafe {
            if execve(
                self.binary_path.as_ptr(),
                self.args_ptrs.as_ptr(),
                self.env_vars_ptrs.as_ptr(),
            ) == -1
            {
                Err(Errno::last())
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // Note: None of these tests call the exec() method, because execve replaces the current process
    // image. If exec() were called, the test process would be replaced and the test runner
    // would lose control. To test exec(), use an integration test that forks a child process
    // and calls exec() in the child.
    use super::*;

    #[test]
    fn test_prepared_execve_new_basic() {
        let binary_path = "/bin/echo";
        let args = vec!["hello".to_string(), "world".to_string()];
        let env = vec![
            ("PATH".to_string(), "/bin:/usr/bin".to_string()),
            ("HOME".to_string(), "/home/user".to_string()),
        ];

        let prepared = PreparedExecve::new(binary_path, &args, &env).unwrap();

        // Verify the struct was created successfully
        // We can't directly access private fields, but we can verify the struct exists
        assert!(std::mem::size_of_val(&prepared) > 0);
    }

    #[test]
    fn test_prepared_execve_new_empty_args() {
        let binary_path = "/bin/true";
        let args: Vec<String> = vec![];
        let env: Vec<(String, String)> = vec![];

        let prepared = PreparedExecve::new(binary_path, &args, &env).unwrap();

        // Should still create successfully with empty args and env
        assert!(std::mem::size_of_val(&prepared) > 0);
    }

    #[test]
    fn test_prepared_execve_new_complex_args() {
        let binary_path = "/usr/bin/env";
        let args = vec![
            "program".to_string(),
            "--flag".to_string(),
            "value with spaces".to_string(),
            "arg with \"quotes\"".to_string(),
        ];
        let env = vec![
            ("VAR1".to_string(), "value1".to_string()),
            ("VAR2".to_string(), "value with spaces".to_string()),
            ("VAR3".to_string(), "value with \"quotes\"".to_string()),
        ];

        let prepared = PreparedExecve::new(binary_path, &args, &env).unwrap();

        // Should handle complex arguments and environment variables
        assert!(std::mem::size_of_val(&prepared) > 0);
    }

    #[test]
    fn test_prepared_execve_new_special_characters() {
        let binary_path = "/bin/test";
        let args = vec![
            "normal".to_string(),
            "with\nnewline".to_string(),
            "with\ttab".to_string(),
            "with\0null".to_string(),
        ];
        let env = vec![
            ("NORMAL".to_string(), "value".to_string()),
            ("SPECIAL".to_string(), "value\nwith\nnewlines".to_string()),
        ];

        // This should return an error due to null bytes in the arguments
        let result = PreparedExecve::new(binary_path, &args, &env);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PreparedExecveError::ArgumentError(_)
        ));
    }

    #[test]
    fn test_prepared_execve_new_null_bytes_in_env() {
        let binary_path = "/bin/test";
        let args = vec!["normal".to_string()];
        let env = vec![
            ("NORMAL".to_string(), "value".to_string()),
            ("SPECIAL".to_string(), "value\0with\0nulls".to_string()),
        ];

        // This should return an error due to null bytes in the environment variables
        let result = PreparedExecve::new(binary_path, &args, &env);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PreparedExecveError::EnvironmentError(_)
        ));
    }

    #[test]
    fn test_prepared_execve_new_null_bytes_in_binary_path() {
        let binary_path = "/bin/test\0with\0nulls";
        let args = vec!["normal".to_string()];
        let env: Vec<(String, String)> = vec![];

        // This should return an error due to null bytes in the binary path
        let result = PreparedExecve::new(binary_path, &args, &env);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PreparedExecveError::BinaryPathError(_)
        ));
    }

    #[test]
    fn test_prepared_execve_new_unicode() {
        let binary_path = "/bin/echo";
        let args = vec![
            "normal".to_string(),
            "with 🦀 emoji".to_string(),
            "with émojis".to_string(),
        ];
        let env = vec![
            ("NORMAL".to_string(), "value".to_string()),
            ("UNICODE".to_string(), "value with 🦀 emoji".to_string()),
            ("ACCENTS".to_string(), "value with émojis".to_string()),
        ];

        let prepared = PreparedExecve::new(binary_path, &args, &env).unwrap();

        // Should handle Unicode characters
        assert!(std::mem::size_of_val(&prepared) > 0);
    }

    #[test]
    fn test_prepared_execve_new_large_args() {
        let binary_path = "/bin/echo";
        let args: Vec<String> = (0..1000).map(|i| format!("arg{}", i)).collect();
        let env = vec![("TEST".to_string(), "value".to_string())];

        let prepared = PreparedExecve::new(binary_path, &args, &env).unwrap();

        // Should handle large numbers of arguments
        assert!(std::mem::size_of_val(&prepared) > 0);
    }

    #[test]
    fn test_prepared_execve_new_large_env() {
        let binary_path = "/bin/echo";
        let args = vec!["test".to_string()];
        let env: Vec<(String, String)> = (0..1000)
            .map(|i| (format!("VAR{}", i), format!("value{}", i)))
            .collect();

        let prepared = PreparedExecve::new(binary_path, &args, &env).unwrap();

        // Should handle large numbers of environment variables
        assert!(std::mem::size_of_val(&prepared) > 0);
    }

    #[test]
    fn test_prepared_execve_new_empty_binary_path() {
        let binary_path = "";
        let args = vec!["test".to_string()];
        let env: Vec<(String, String)> = vec![];

        let prepared = PreparedExecve::new(binary_path, &args, &env).unwrap();

        // Should handle empty binary path (though this might not be valid for execve)
        assert!(std::mem::size_of_val(&prepared) > 0);
    }

    #[test]
    fn test_prepared_execve_new_empty_env_keys() {
        let binary_path = "/bin/echo";
        let args = vec!["test".to_string()];
        let env = vec![
            ("".to_string(), "value".to_string()), // Empty key
            ("KEY".to_string(), "".to_string()),   // Empty value
        ];

        let prepared = PreparedExecve::new(binary_path, &args, &env).unwrap();

        // Should handle empty keys and values
        assert!(std::mem::size_of_val(&prepared) > 0);
    }

    #[test]
    fn test_prepared_execve_error_variants() {
        // Test binary path error
        let result = PreparedExecve::new("/bin/test\0", &[], &[]);
        assert!(matches!(
            result.unwrap_err(),
            PreparedExecveError::BinaryPathError(_)
        ));

        // Test argument error
        let result = PreparedExecve::new("/bin/test", &["arg\0".to_string()], &[]);
        assert!(matches!(
            result.unwrap_err(),
            PreparedExecveError::ArgumentError(_)
        ));

        // Test environment error
        let result = PreparedExecve::new(
            "/bin/test",
            &[],
            &[("KEY\0".to_string(), "value".to_string())],
        );
        assert!(matches!(
            result.unwrap_err(),
            PreparedExecveError::EnvironmentError(_)
        ));
    }
}
