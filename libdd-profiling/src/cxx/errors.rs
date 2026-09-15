// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::ffi;

pub(crate) use super::ffi::Operation;

impl Operation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CreateProfile => "Profile::create",
            Self::CreateProfileWithDictionary => "Profile::create_with_dictionary",
            Self::AddSample => "Profile::add_sample",
            Self::AddSampleWithTimestamp => "Profile::add_sample_with_timestamp",
            Self::AddDictionarySample => "Profile::add_dictionary_sample",
            Self::SetCustomSampleType => "Profile::set_custom_sample_type",
            Self::AddEndpoint => "Profile::add_endpoint",
            Self::AddEndpointCount => "Profile::add_endpoint_count",
            Self::AddUpscalingRulePoisson => "Profile::add_upscaling_rule_poisson",
            Self::AddUpscalingRulePoissonNonSampleTypeCount => {
                "Profile::add_upscaling_rule_poisson_non_sample_type_count"
            }
            Self::AddUpscalingRuleProportional => "Profile::add_upscaling_rule_proportional",
            Self::SerializeProfile => "Profile::serialize",
            Self::CreateProfileDictionary => "ProfileDictionary::create",
            Self::InternDictionaryString => "ProfileDictionary::intern_string",
            Self::InternDictionaryFunction => "ProfileDictionary::intern_function",
            Self::InternDictionaryMapping => "ProfileDictionary::intern_mapping",
            Self::CreateAgentExporter => "ProfileExporter::create_agent_exporter",
            Self::CreateAgentlessExporter => "ProfileExporter::create_agentless_exporter",
            Self::CreateFileExporter => "ProfileExporter::create_file_exporter",
            Self::SendEncodedProfile => "ProfileExporter::send_encoded_profile",
            Self::SendEncodedProfileWithCancellation => {
                "ProfileExporter::send_encoded_profile_with_cancellation"
            }
            _ => "unknown operation",
        }
    }
}

impl ffi::Status {
    pub(crate) fn ok_for(operation: Operation) -> Self {
        Self {
            success: true,
            operation,
            details: String::new(),
        }
    }

    pub(crate) fn err(operation: Operation, err: impl std::fmt::Display) -> Self {
        Self {
            success: false,
            operation,
            details: format!("{err:#}"),
        }
    }

    pub(crate) fn from_result<E>(operation: Operation, result: std::result::Result<(), E>) -> Self
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

    pub fn operation_name(&self) -> String {
        self.operation.as_str().to_string()
    }

    pub fn message(&self) -> String {
        self.details.clone()
    }

    pub fn check_and_print(&self) -> bool {
        if self.success {
            return true;
        }
        eprintln!("{} failed: {}", self.operation.as_str(), self.details);
        false
    }

    #[cfg(test)]
    #[track_caller]
    pub fn unwrap(self) {
        assert!(
            self.success,
            "{} failed: {}",
            self.operation.as_str(),
            self.details
        );
    }
}

impl ffi::Error {
    pub fn operation_name(&self) -> String {
        self.operation.as_str().to_string()
    }
}

macro_rules! impl_box_result {
    ($result:ident, $value:ty) => {
        #[must_use]
        pub struct $result {
            status: ffi::Status,
            value: Option<Box<$value>>,
        }

        impl $result {
            pub(crate) fn from_result(
                operation: Operation,
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

            pub fn take_value(&mut self) -> Box<$value> {
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
impl_box_result_with_message!(EncodedProfileResult, super::profile::EncodedProfile);
impl_box_result_with_message!(
    ProfileExporterResult,
    super::profile_exporter::ProfileExporter
);

pub(crate) struct ErrorStore {
    policy: ffi::ErrorPolicy,
    errors: Vec<ffi::Error>,
    printed_operations: Vec<Operation>,
}

impl ErrorStore {
    pub(crate) fn new() -> Self {
        Self {
            policy: ffi::ErrorPolicy::StoreFirstPerOperation,
            errors: Vec::new(),
            printed_operations: Vec::new(),
        }
    }

    pub(crate) fn handle_result(
        &mut self,
        operation: Operation,
        result: anyhow::Result<()>,
    ) -> bool {
        match result {
            Ok(()) => true,
            Err(err) => self.handle_error(operation, &err),
        }
    }

    pub(crate) fn handle_error(
        &mut self,
        operation: Operation,
        err: impl std::fmt::Display,
    ) -> bool {
        match self.policy {
            ffi::ErrorPolicy::PrintImmediately => {
                eprintln!("{} failed: {err:#}", operation.as_str());
            }
            ffi::ErrorPolicy::PrintOncePerOperation => {
                if !self.printed_operations.contains(&operation) {
                    self.printed_operations.push(operation);
                    eprintln!("{} failed: {err:#}", operation.as_str());
                }
            }
            ffi::ErrorPolicy::StoreFirstPerOperation => {
                if !self.errors.iter().any(|error| error.operation == operation) {
                    self.errors.push(ffi::Error {
                        operation,
                        message: format!("{err:#}"),
                    });
                }
            }
            ffi::ErrorPolicy::StoreEveryOccurrence => self.errors.push(ffi::Error {
                operation,
                message: format!("{err:#}"),
            }),
            _ => {
                eprintln!("{} failed: {err:#}", operation.as_str());
            }
        }
        false
    }

    pub(crate) fn set_policy(&mut self, policy: ffi::ErrorPolicy) {
        self.policy = policy;
    }

    pub(crate) fn take_errors(&mut self) -> Vec<ffi::Error> {
        std::mem::take(&mut self.errors)
    }

    #[cfg(test)]
    pub(crate) fn printed_operation_count(&self) -> usize {
        self.printed_operations.len()
    }
}
