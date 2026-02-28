// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collections::identifiable::{FxIndexMap, StringId};
use crate::collections::string_table::StringTable;
use crate::profiles::collections::StringRef;
use crate::profiles::datatypes::ProfilesDictionary;
use indexmap::map::Entry;

/// Translates dictionary-backed strings into profile-local string table ids.
///
/// # Safety
///
/// All ids passed to [`ProfilesDictionaryTranslator::translate_string`] MUST
/// have been created by the same [`ProfilesDictionary`] that this translator
/// wraps.
pub struct ProfilesDictionaryTranslator {
    pub profiles_dictionary: crate::profiles::collections::Arc<ProfilesDictionary>,
    pub strings: FxIndexMap<StringRef, StringId>,
}

// SAFETY: ProfilesDictionaryTranslator is Send because:
// 1. The profiles_dictionary Arc ensures the underlying storage remains alive and valid for the
//    lifetime of this translator, and Arc<T> is Send when T is Send + Sync. ProfilesDictionary is
//    Send + Sync.
// 2. SetId<T> and StringRef are non-owning handles (thin pointers) to immutable data in the
//    ProfilesDictionary's concurrent collections, which use arena allocation with stable addresses.
//    The Arc protects this data, making the pointers safe to send across threads.
// 3. FxIndexMap<K, V> is Send when K and V are Send. The keys (StringRef) and values (StringId) are
//    Copy types that are Send.
unsafe impl Send for ProfilesDictionaryTranslator {}

impl ProfilesDictionaryTranslator {
    pub fn new(
        profiles_dictionary: crate::profiles::collections::Arc<ProfilesDictionary>,
    ) -> ProfilesDictionaryTranslator {
        ProfilesDictionaryTranslator {
            profiles_dictionary,
            strings: Default::default(),
        }
    }

    /// Translates a StringRef from the ProfilesDictionary into a StringId
    /// for this profile's internal string table.
    ///
    /// # Safety
    ///
    /// The `str_ref` must have been created by `self.profiles_dictionary`.
    /// Violating this precondition results in undefined behavior.
    pub unsafe fn translate_string(
        &mut self,
        string_table: &mut StringTable,
        str_ref: StringRef,
    ) -> anyhow::Result<StringId> {
        self.strings.try_reserve(1)?;
        match self.strings.entry(str_ref) {
            Entry::Occupied(o) => Ok(*o.get()),
            Entry::Vacant(v) => {
                // SAFETY: This is safe if `str_ref` was created by
                // `self.profiles_dictionary`, which is a precondition of calling
                // this method.
                let str = unsafe { self.profiles_dictionary.strings().get(str_ref) };
                let internal_id = string_table.try_intern(str)?;
                v.insert(internal_id);
                Ok(internal_id)
            }
        }
    }
}
