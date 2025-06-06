use std::num::NonZeroI64;

use crate::profiles::datatypes::{ProfileResult, Sample};
use anyhow::Context;
use datadog_profiling::internal;

/// # Safety
/// The `profile` ptr must point to a valid internal::Profile object.
/// All pointers inside the `sample` need to be valid for the duration of this call.
/// This call is _NOT_ thread-safe.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_add_internal(
    profile: *mut internal::Profile,
    sample: Sample,
    timestamp: Option<NonZeroI64>,
) -> ProfileResult {
    (|| {
        let profile = profile
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("profile pointer was null"))?;
        let uses_string_ids = sample
            .labels
            .first()
            .is_some_and(|label| label.key.is_empty() && label.key_id.value > 0);

        if uses_string_ids {
            profile.add_string_id_sample(sample.into(), timestamp)
        } else {
            profile.add_sample(sample.try_into()?, timestamp)
        }
    })()
    .context("ddog_prof_Profile_add_internal failed")
    .into()
}
