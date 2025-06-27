use crate::profiles::ProfileError;
use datadog_alloc::Box;
use datadog_profiling::{
    collections::string_table::StringTable, ProfileVoidResult,
};
use datadog_profiling_protobuf::StringOffset;
use ddcommon_ffi::CharSlice;
use std::{borrow::Cow, collections::HashMap, ptr};

/// Manages endpoint mappings for profiling.
///
/// This struct stores mappings from local root span IDs to endpoint names.
/// When samples contain a "local root span id" label, the corresponding
/// endpoint name will be automatically added as a "trace endpoint" label
/// during profile serialization.
pub struct Endpoints {
    /// Maps local root span IDs to string offsets in the string table
    mappings: HashMap<u64, StringOffset>,
    /// String table for storing endpoint names
    strings: StringTable,
}

impl Endpoints {
    /// Creates a new empty Endpoints instance.
    ///
    /// # Errors
    /// Returns an error if memory allocation fails.
    pub fn try_new() -> Result<Self, ProfileError> {
        Ok(Self { mappings: HashMap::new(), strings: StringTable::try_new()? })
    }

    /// Adds a mapping from a local root span ID to an endpoint name.
    ///
    /// # Arguments
    /// * `local_root_span_id` - The span ID to map
    /// * `endpoint` - The endpoint name to associate with this span ID
    ///
    /// # Errors
    /// Returns an error if memory allocation fails.
    pub fn add_endpoint(
        &mut self,
        local_root_span_id: u64,
        endpoint: &str,
    ) -> Result<(), ProfileError> {
        self.mappings.try_reserve(1).map_err(ProfileError::from)?;
        let string_offset = self.strings.try_intern(endpoint)?;
        self.mappings.insert(local_root_span_id, string_offset);
        Ok(())
    }

    /// Gets the string offset for the endpoint name associated with a given local root span ID.
    ///
    /// # Arguments
    /// * `local_root_span_id` - The span ID to look up
    ///
    /// # Returns
    /// The string offset if found, None otherwise
    pub fn get_endpoint(
        &self,
        local_root_span_id: u64,
    ) -> Option<StringOffset> {
        self.mappings.get(&local_root_span_id).copied()
    }

    /// Gets the string table containing endpoint names.
    pub fn strings(&self) -> &StringTable {
        &self.strings
    }
}

// FFI functions for Endpoints

/// Creates a new Endpoints instance.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_prof_Endpoints_new() -> *mut Endpoints {
    match Endpoints::try_new() {
        Ok(endpoints) => match Box::try_new(endpoints) {
            Ok(boxed) => Box::into_raw(boxed),
            Err(_) => ptr::null_mut(),
        },
        Err(_) => ptr::null_mut(),
    }
}

/// Adds a mapping from a local root span ID to an endpoint name.
///
/// # Safety
///
/// The `endpoints` must be a valid pointer to an Endpoints instance.
/// If `assume_utf8` is true, then `endpoint` must be valid UTF-8.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_Endpoints_add(
    endpoints: *mut Endpoints,
    local_root_span_id: u64,
    endpoint: CharSlice,
    assume_utf8: bool,
) -> ProfileVoidResult {
    let Some(endpoints) = endpoints.as_mut() else {
        return ProfileError::InvalidInput.into();
    };

    let Some(endpoint_slice) = endpoint.try_as_slice() else {
        return ProfileError::InvalidInput.into();
    };

    // SAFETY: convert from &[c_char] to &[u8].
    let endpoint_bytes = unsafe {
        std::slice::from_raw_parts(
            endpoint_slice.as_ptr(),
            endpoint_slice.len(),
        )
    };

    let endpoint_str = if assume_utf8 {
        // SAFETY: caller guarantees endpoint is valid UTF-8.
        Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(endpoint_bytes) })
    } else {
        // Do lossy conversion like the string table APIs
        String::from_utf8_lossy(endpoint_bytes)
    };

    match endpoints.add_endpoint(local_root_span_id, &endpoint_str) {
        Ok(()) => ProfileVoidResult::Ok,
        Err(err) => ProfileVoidResult::Err(err),
    }
}

/// Drops an Endpoints instance.
///
/// # Safety
///
/// The `endpoints` must be a valid pointer to a pointer to an Endpoints instance.
/// After this call, the pointer will be set to null.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Endpoints_drop(
    endpoints: *mut *mut Endpoints,
) {
    if !endpoints.is_null() && !(*endpoints).is_null() {
        drop(Box::from_raw(*endpoints));
        *endpoints = ptr::null_mut();
    }
}
