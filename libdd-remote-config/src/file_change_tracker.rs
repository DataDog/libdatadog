// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::RemoteConfigPath;
use crate::fetch::HeldFile;
use std::collections::HashMap;
use std::sync::Arc;

pub trait FilePath {
    fn path(&self) -> &RemoteConfigPath;
}

pub enum Change<S, R> {
    Add(S),
    /// The new file, and the one it replaces at the same path.
    Update(S, R),
    Remove(S),
}

/// Computes the changes from the files a client held before a fetch to those it holds after it
/// (see [`crate::fetch::Fetched`]). Stored files are never modified: a different file at the same
/// path is a new version, reported as [`Change::Update`] with the previous file, while the same
/// file (a version-only change) is no change.
pub fn changes<F>(
    previous: &[HeldFile<F>],
    current: &[HeldFile<F>],
) -> Vec<Change<Arc<F>, Arc<F>>> {
    fn by_path<F>(files: &[HeldFile<F>]) -> HashMap<&RemoteConfigPath, &Arc<F>> {
        files.iter().map(|held| (&*held.path, &held.file)).collect()
    }
    let before = by_path(previous);
    let after = by_path(current);
    let mut changes = vec![];

    // Emit removals before additions/updates so that consumers observe
    // configuration deletions strictly before new or updated configs.
    for (path, file) in before.iter() {
        if !after.contains_key(path) {
            changes.push(Change::Remove((*file).clone()));
        }
    }

    for (path, file) in after.iter() {
        if !before.contains_key(path) {
            changes.push(Change::Add((*file).clone()));
        }
    }

    for (path, file) in after.iter() {
        if let Some(old) = before.get(path)
            && !Arc::ptr_eq(file, old)
        {
            changes.push(Change::Update((*file).clone(), (*old).clone()));
        }
    }

    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RemoteConfigPath;

    struct TestFile(Arc<RemoteConfigPath>);

    fn file(name: &str) -> HeldFile<TestFile> {
        let path = Arc::new(
            RemoteConfigPath::parse(&format!("employee/APM_TRACING/id/{name}"))
                .expect("test file has a valid path"),
        );
        HeldFile {
            path: path.clone(),
            version: 1,
            file: Arc::new(TestFile(path)),
        }
    }

    #[test]
    fn removals_are_emitted_before_additions() {
        // "a" is gone, "b" appears. The removal of "a" must be emitted before the addition of
        // "b".
        let a = file("a");
        let b = file("b");
        let changes = changes(&[a], &[b]);

        assert_eq!(changes.len(), 2);
        match &changes[0] {
            Change::Remove(f) => assert_eq!(f.0.name(), "a"),
            _ => panic!("expected the removal to be emitted first"),
        }
        match &changes[1] {
            Change::Add(f) => assert_eq!(f.0.name(), "b"),
            _ => panic!("expected the addition to be emitted after the removal"),
        }
    }

    #[test]
    fn update_carries_the_previous_file_after_removals() {
        let a1 = file("a");
        let b = file("b");

        // Holding the same files is no change, whatever their version.
        let a1_v2 = HeldFile {
            version: 2,
            ..a1.clone()
        };
        assert!(changes(&[a1.clone(), b.clone()], &[a1_v2, b.clone()]).is_empty());

        // A new version of "a" is a new file at the same path; "b" is gone.
        let a2 = file("a");
        let changes = changes(&[a1.clone(), b.clone()], std::slice::from_ref(&a2));

        assert_eq!(changes.len(), 2);
        match &changes[0] {
            Change::Remove(f) => assert!(Arc::ptr_eq(f, &b.file)),
            _ => panic!("expected the removal to be emitted first"),
        }
        match &changes[1] {
            Change::Update(new, old) => {
                assert!(
                    Arc::ptr_eq(new, &a2.file),
                    "the update carries the new file"
                );
                assert!(
                    Arc::ptr_eq(old, &a1.file),
                    "the update carries the previous file"
                );
            }
            _ => panic!("expected the update to be emitted after the removal"),
        }
    }
}
