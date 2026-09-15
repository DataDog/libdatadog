// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::errors::{ErrorStore, ProfileDictionaryResult};
use super::ffi;
use super::ids::{dictionary_function_from_cxx, dictionary_mapping_from_cxx};
use crate::profiles;
use anyhow::Context;

pub struct ProfileDictionary {
    pub(crate) inner: profiles::collections::Arc<profiles::datatypes::ProfilesDictionary>,
    errors: std::sync::Mutex<ErrorStore>,
}

impl ProfileDictionary {
    pub fn create() -> Box<ProfileDictionaryResult> {
        ProfileDictionaryResult::from_result(
            "ProfileDictionary::create",
            (|| -> anyhow::Result<Box<ProfileDictionary>> {
                let dictionary = profiles::datatypes::ProfilesDictionary::try_new()
                    .context("ProfileDictionary::create failed")?;
                let inner = profiles::collections::Arc::try_new(dictionary)
                    .map_err(|_| anyhow::anyhow!("failed to allocate ProfileDictionary"))?;

                Ok(Box::new(ProfileDictionary {
                    inner,
                    errors: std::sync::Mutex::new(ErrorStore::new()),
                }))
            })(),
        )
    }

    pub(crate) fn handle_error(&self, operation: &'static str, err: impl std::fmt::Display) {
        match self.errors.lock() {
            Ok(mut errors) => {
                errors.handle_error(operation, err);
            }
            Err(_) => {
                eprintln!("{operation} failed: {err:#}");
            }
        }
    }

    pub fn set_error_policy(&self, policy: ffi::ErrorPolicy) {
        if let Ok(mut errors) = self.errors.lock() {
            errors.set_policy(policy);
        }
    }

    pub fn error_policy(&self) -> ffi::ErrorPolicy {
        match self.errors.lock() {
            Ok(errors) => errors.policy(),
            Err(_) => ffi::ErrorPolicy::StoreFirstPerOperation,
        }
    }

    pub fn take_errors(&self) -> Vec<ffi::Error> {
        match self.errors.lock() {
            Ok(mut errors) => errors.take_errors(),
            Err(_) => Vec::new(),
        }
    }

    fn write_output<T, E>(
        &self,
        operation: &'static str,
        out: &mut T,
        result: std::result::Result<T, E>,
    ) -> bool
    where
        T: Default,
        E: std::fmt::Display,
    {
        match result {
            Ok(value) => {
                *out = value;
                true
            }
            Err(err) => {
                *out = T::default();
                self.handle_error(operation, &err);
                false
            }
        }
    }

    pub fn intern_string(&self, value: &str, out: &mut ffi::DictionaryStringId) -> bool {
        self.write_output(
            "ProfileDictionary::intern_string",
            out,
            self.inner.try_insert_str2(value).map(Into::into),
        )
    }

    pub fn intern_function(
        &self,
        function: &ffi::DictionaryFunction,
        out: &mut ffi::DictionaryFunctionId,
    ) -> bool {
        // SAFETY: The CXX API contract requires all ids in function to come
        // from this ProfileDictionary.
        let function = unsafe { dictionary_function_from_cxx(function) };
        self.write_output(
            "ProfileDictionary::intern_function",
            out,
            self.inner.try_insert_function2(function).map(Into::into),
        )
    }

    pub fn intern_mapping(
        &self,
        mapping: &ffi::DictionaryMapping,
        out: &mut ffi::DictionaryMappingId,
    ) -> bool {
        // SAFETY: The CXX API contract requires all ids in mapping to come
        // from this ProfileDictionary.
        let mapping = unsafe { dictionary_mapping_from_cxx(mapping) };
        self.write_output(
            "ProfileDictionary::intern_mapping",
            out,
            self.inner.try_insert_mapping2(mapping).map(Into::into),
        )
    }
}
