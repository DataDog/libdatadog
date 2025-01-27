// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::atomic_set::AtomicStringSet;
use std::io::Write;

static ADDITIONAL_TAGS: AtomicStringSet<2048> = AtomicStringSet::new();

pub fn clear_additional_tags() -> anyhow::Result<()> {
    ADDITIONAL_TAGS.clear()
}

pub fn consume_and_emit_additional_tags(w: &mut impl Write) -> anyhow::Result<()> {
    use crate::shared::constants::*;
    writeln!(w, "{DD_CRASHTRACK_BEGIN_ADDITIONAL_TAGS}")?;
    ADDITIONAL_TAGS.consume_and_emit(w, true)?;
    writeln!(w, "{DD_CRASHTRACK_END_ADDITIONAL_TAGS}")?;
    w.flush()?;
    Ok(())
}

pub fn insert_additional_tag(value: String) -> anyhow::Result<usize> {
    ADDITIONAL_TAGS.insert(value)
}

pub fn remove_additional_tag(idx: usize) -> anyhow::Result<()> {
    ADDITIONAL_TAGS.remove(idx)
}
