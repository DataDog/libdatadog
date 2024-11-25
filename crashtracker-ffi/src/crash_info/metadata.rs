use ddcommon::tag::Tag;
use ddcommon_ffi::{slice::AsBytes, CharSlice};

#[repr(C)]
pub struct Metadata<'a> {
    pub library_name: CharSlice<'a>,
    pub library_version: CharSlice<'a>,
    pub family: CharSlice<'a>,
    /// Should include "service", "environment", etc
    pub tags: Option<&'a ddcommon_ffi::Vec<Tag>>,
}

impl<'a> TryFrom<Metadata<'a>> for datadog_crashtracker::rfc5_crash_info::Metadata {
    type Error = anyhow::Error;
    fn try_from(value: Metadata<'a>) -> anyhow::Result<Self> {
        let library_name = value.library_name.try_to_utf8()?.to_string();
        let library_version = value.library_version.try_to_utf8()?.to_string();
        let family = value.family.try_to_utf8()?.to_string();
        let tags = if let Some(tags) = value.tags {
            tags.into_iter().map(|t| t.to_string()).collect()
        } else {
            vec![]
        };
        Ok(Self {
            library_name,
            library_version,
            family,
            tags,
        })
    }
}

impl<'a> TryFrom<Metadata<'a>> for datadog_crashtracker::CrashtrackerMetadata {
    type Error = anyhow::Error;
    fn try_from(value: Metadata<'a>) -> anyhow::Result<Self> {
        let library_name = value.library_name.try_to_utf8()?.to_string();
        let library_version = value.library_version.try_to_utf8()?.to_string();
        let family = value.family.try_to_utf8()?.to_string();
        let tags = value
            .tags
            .map(|tags| tags.iter().cloned().collect())
            .unwrap_or_default();
        Ok(Self {
            library_name,
            library_version,
            family,
            tags,
        })
    }
}
