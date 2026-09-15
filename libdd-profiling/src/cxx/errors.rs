// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::ffi;

impl ffi::Status {
    pub(crate) fn ok_for(operation: &'static str) -> Self {
        Self {
            success: true,
            operation_name: operation.to_string(),
            details: String::new(),
        }
    }

    pub(crate) fn err(operation: &'static str, err: impl std::fmt::Display) -> Self {
        Self {
            success: false,
            operation_name: operation.to_string(),
            details: format!("{err:#}"),
        }
    }

    pub(crate) fn from_result<E>(
        operation: &'static str,
        result: std::result::Result<(), E>,
    ) -> Self
    where
        E: std::fmt::Display,
    {
        match result {
            Ok(()) => Self::ok_for(operation),
            Err(err) => Self::err(operation, err),
        }
    }

    pub fn ok(&self) -> bool {
        self.success
    }

    pub fn operation(&self) -> String {
        self.operation_name.clone()
    }

    pub fn message(&self) -> String {
        self.details.clone()
    }

    pub fn check_and_print(&self) -> bool {
        if self.success {
            return true;
        }
        eprintln!("{} failed: {}", self.operation_name, self.details);
        false
    }

    pub fn check_and_store_first_per_operation(&self, errors: &mut Vec<ffi::Error>) -> bool {
        if self.success {
            return true;
        }
        if !errors
            .iter()
            .any(|error| error.operation == self.operation_name)
        {
            errors.push(ffi::Error {
                operation: self.operation_name.clone(),
                message: self.details.clone(),
            });
        }
        false
    }

    pub fn check_and_store_every_occurrence(&self, errors: &mut Vec<ffi::Error>) -> bool {
        if self.success {
            return true;
        }
        errors.push(ffi::Error {
            operation: self.operation_name.clone(),
            message: self.details.clone(),
        });
        false
    }

    #[cfg(test)]
    #[track_caller]
    pub fn unwrap(self) {
        assert!(self.success, "{}", self.details);
    }
}

macro_rules! impl_box_result {
    ($result:ident, $value:ty) => {
        #[must_use]
        pub struct $result {
            pub(crate) status: ffi::Status,
            value: Option<Box<$value>>,
        }

        impl $result {
            pub(crate) fn from_result(
                operation: &'static str,
                result: anyhow::Result<Box<$value>>,
            ) -> Box<Self> {
                match result {
                    Ok(value) => Box::new(Self {
                        status: ffi::Status::ok_for(operation),
                        value: Some(value),
                    }),
                    Err(err) => Box::new(Self {
                        status: ffi::Status::err(operation, err),
                        value: None,
                    }),
                }
            }

            pub fn ok(&self) -> bool {
                self.status.ok()
            }

            pub fn check_and_print(&self) -> bool {
                self.status.check_and_print()
            }

            pub fn check_and_store_first_per_operation(
                &self,
                errors: &mut Vec<ffi::Error>,
            ) -> bool {
                self.status.check_and_store_first_per_operation(errors)
            }

            pub fn check_and_store_every_occurrence(&self, errors: &mut Vec<ffi::Error>) -> bool {
                self.status.check_and_store_every_occurrence(errors)
            }

            pub fn take(&mut self) -> Box<$value> {
                match self.value.take() {
                    Some(value) => value,
                    None => std::process::abort(),
                }
            }

            #[cfg(test)]
            #[track_caller]
            pub fn is_ok(&self) -> bool {
                self.status.ok()
            }

            #[cfg(test)]
            #[track_caller]
            pub fn is_err(&self) -> bool {
                !self.status.ok()
            }

            #[cfg(test)]
            #[allow(clippy::boxed_local)]
            #[track_caller]
            pub fn unwrap(mut self: Box<Self>) -> Box<$value> {
                self.status.unwrap();
                self.value.take().expect("successful result has value")
            }
        }
    };
}

macro_rules! impl_box_result_with_message {
    ($result:ident, $value:ty) => {
        impl_box_result!($result, $value);

        impl $result {
            pub fn message(&self) -> String {
                self.status.message()
            }
        }
    };
}

impl_box_result_with_message!(ProfileResult, super::profile::Profile);
impl_box_result_with_message!(
    ProfileDictionaryResult,
    super::dictionary::ProfileDictionary
);
impl_box_result!(EncodedProfileResult, super::profile::EncodedProfile);
impl_box_result_with_message!(
    ProfileExporterResult,
    super::profile_exporter::ProfileExporter
);
impl_box_result_with_message!(
    ExporterManagerResult,
    super::exporter_manager::ExporterManager
);

pub(crate) struct ErrorStore {
    policy: ffi::ErrorPolicy,
    errors: Vec<ffi::Error>,
}

impl ErrorStore {
    pub(crate) fn new() -> Self {
        Self {
            policy: ffi::ErrorPolicy::StoreFirstPerOperation,
            errors: Vec::new(),
        }
    }

    pub(crate) fn handle_result(
        &mut self,
        operation: &'static str,
        result: anyhow::Result<()>,
    ) -> bool {
        match result {
            Ok(()) => true,
            Err(err) => self.handle_error(operation, &err),
        }
    }

    pub(crate) fn handle_error(
        &mut self,
        operation: &'static str,
        err: impl std::fmt::Display,
    ) -> bool {
        match self.policy {
            ffi::ErrorPolicy::PrintImmediately => {
                eprintln!("{operation} failed: {err:#}");
            }
            ffi::ErrorPolicy::StoreFirstPerOperation => {
                if !self.errors.iter().any(|error| error.operation == operation) {
                    self.errors.push(ffi::Error {
                        operation: operation.to_string(),
                        message: format!("{err:#}"),
                    });
                }
            }
            ffi::ErrorPolicy::StoreEveryOccurrence => self.errors.push(ffi::Error {
                operation: operation.to_string(),
                message: format!("{err:#}"),
            }),
            _ => {
                eprintln!("{operation} failed: {err:#}");
            }
        }
        false
    }

    pub(crate) fn set_policy(&mut self, policy: ffi::ErrorPolicy) {
        self.policy = policy;
    }

    pub(crate) fn policy(&self) -> ffi::ErrorPolicy {
        self.policy
    }

    pub(crate) fn errors(&self) -> Vec<ffi::Error> {
        self.errors
            .iter()
            .map(|error| ffi::Error {
                operation: error.operation.clone(),
                message: error.message.clone(),
            })
            .collect()
    }

    pub(crate) fn take_errors(&mut self) -> Vec<ffi::Error> {
        std::mem::take(&mut self.errors)
    }

    pub(crate) fn clear_errors(&mut self) {
        self.errors.clear();
    }
}
