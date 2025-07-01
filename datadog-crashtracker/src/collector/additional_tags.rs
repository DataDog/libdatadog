// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::atomic_set::{AtomicSetError, AtomicStringMultiset};
use std::io::Write;

static ADDITIONAL_TAGS: AtomicStringMultiset<2048> = AtomicStringMultiset::new();

pub fn clear_additional_tags() -> Result<(), AtomicSetError> {
    ADDITIONAL_TAGS.clear()
}

pub fn consume_and_emit_additional_tags(w: &mut impl Write) -> Result<(), AtomicSetError> {
    use crate::shared::constants::*;
    writeln!(w, "{DD_CRASHTRACK_BEGIN_ADDITIONAL_TAGS}")?;
    ADDITIONAL_TAGS.consume_and_emit(w, true)?;
    writeln!(w, "{DD_CRASHTRACK_END_ADDITIONAL_TAGS}")?;
    w.flush()?;
    Ok(())
}

pub fn insert_additional_tag(value: String) -> Result<usize, AtomicSetError> {
    ADDITIONAL_TAGS.insert(value)
}

pub fn remove_additional_tag(idx: usize) -> Result<(), AtomicSetError> {
    ADDITIONAL_TAGS.remove(idx)
}
