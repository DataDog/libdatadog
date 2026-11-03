// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::RemoteConfigPath;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

pub trait FilePath {
    fn path(&self) -> &RemoteConfigPath;
}

struct FilePathBasedArc<S: FilePath>(Arc<S>);

impl<S: FilePath> Hash for FilePathBasedArc<S> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.path().hash(state)
    }
}

impl<S: FilePath> PartialEq for FilePathBasedArc<S> {
    fn eq(&self, other: &Self) -> bool {
        self.0.path() == other.0.path()
    }
}

impl<S: FilePath> Eq for FilePathBasedArc<S> {}

pub struct ChangeTracker<S: FilePath> {
    last_files: HashSet<FilePathBasedArc<S>>,
}

impl<S: FilePath> Default for ChangeTracker<S> {
    fn default() -> Self {
        ChangeTracker {
            last_files: Default::default(),
        }
    }
}

pub enum Change<S, R> {
    Add(S),
    /// The new file, and the one it replaces at the same path.
    Update(S, R),
    Remove(S),
}

impl<S: FilePath> ChangeTracker<S> {
    /// Computes the changes from the files of the previous call to `files`. Stored files are never
    /// modified: a different file at the same path is a new version, reported as
    /// [`Change::Update`] with the previous file.
    pub fn get_changes(&mut self, files: Vec<Arc<S>>) -> Vec<Change<Arc<S>, Arc<S>>> {
        let files = HashSet::from_iter(files.into_iter().map(FilePathBasedArc));
        let mut changes = vec![];

        // Emit removals before additions/updates so that consumers observe
        // configuration deletions strictly before new or updated configs.
        for file in self.last_files.difference(&files) {
            changes.push(Change::Remove(file.0.clone()));
        }

        for file in files.difference(&self.last_files) {
            changes.push(Change::Add(file.0.clone()));
        }

        for file in files.iter() {
            if let Some(old) = self.last_files.get(file)
                && !Arc::ptr_eq(&file.0, &old.0)
            {
                changes.push(Change::Update(file.0.clone(), old.0.clone()));
            }
        }

        self.last_files = files;
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RemoteConfigPath;

    struct TestFile(RemoteConfigPath);

    impl FilePath for TestFile {
        fn path(&self) -> &RemoteConfigPath {
            &self.0
        }
    }

    fn file(name: &str) -> Arc<TestFile> {
        Arc::new(TestFile(
            RemoteConfigPath::parse(&format!("employee/APM_TRACING/id/{name}"))
                .expect("test file has a valid path"),
        ))
    }

    #[test]
    fn removals_are_emitted_before_additions() {
        let mut tracker: ChangeTracker<TestFile> = ChangeTracker::default();
        let a = file("a");
        // First round: "a" is added.
        tracker.get_changes(vec![a.clone()]);

        // Second round: "a" is gone, "b" appears. The removal of "a" must be
        // emitted before the addition of "b".
        let b = file("b");
        let changes = tracker.get_changes(vec![b.clone()]);

        assert_eq!(changes.len(), 2);
        match &changes[0] {
            Change::Remove(f) => assert_eq!(f.path().name(), "a"),
            _ => panic!("expected the removal to be emitted first"),
        }
        match &changes[1] {
            Change::Add(f) => assert_eq!(f.path().name(), "b"),
            _ => panic!("expected the addition to be emitted after the removal"),
        }
    }

    #[test]
    fn update_carries_the_previous_file_after_removals() {
        let mut tracker: ChangeTracker<TestFile> = ChangeTracker::default();
        let a1 = file("a");
        let b = file("b");
        tracker.get_changes(vec![a1.clone(), b.clone()]);

        // Re-delivering the same files is no change.
        assert!(tracker.get_changes(vec![a1.clone(), b.clone()]).is_empty());

        // A new version of "a" is a new file at the same path; "b" is gone.
        let a2 = file("a");
        let changes = tracker.get_changes(vec![a2.clone()]);

        assert_eq!(changes.len(), 2);
        match &changes[0] {
            Change::Remove(f) => assert!(Arc::ptr_eq(f, &b)),
            _ => panic!("expected the removal to be emitted first"),
        }
        match &changes[1] {
            Change::Update(new, old) => {
                assert!(Arc::ptr_eq(new, &a2), "the update carries the new file");
                assert!(
                    Arc::ptr_eq(old, &a1),
                    "the update carries the previous file"
                );
            }
            _ => panic!("expected the update to be emitted after the removal"),
        }
    }
}
