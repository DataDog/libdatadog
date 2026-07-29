// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::RemoteConfigPath;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

pub trait FilePath {
    fn path(&self) -> &RemoteConfigPath;
}

pub trait UpdatedFiles<S: FilePath, R> {
    fn updated(&self) -> Vec<(Arc<S>, R)>;
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
    Update(S, R),
    Remove(S),
}

impl<S: FilePath> ChangeTracker<S> {
    pub fn get_changes<R>(
        &mut self,
        files: Vec<Arc<S>>,
        updated: Vec<(Arc<S>, R)>,
    ) -> Vec<Change<Arc<S>, R>> {
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

        for (updated_file, old_contents) in updated.into_iter() {
            let file = FilePathBasedArc(updated_file);
            if files.contains(&file) {
                changes.push(Change::Update(file.0, old_contents))
            }
        }

        self.last_files = files;
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RemoteConfigPath, RemoteConfigProduct, RemoteConfigSource};

    struct TestFile(RemoteConfigPath);

    impl FilePath for TestFile {
        fn path(&self) -> &RemoteConfigPath {
            &self.0
        }
    }

    fn file(name: &str) -> Arc<TestFile> {
        Arc::new(TestFile(RemoteConfigPath {
            source: RemoteConfigSource::Employee,
            product: RemoteConfigProduct::ApmTracing,
            config_id: "id".to_string(),
            name: name.to_string(),
        }))
    }

    #[test]
    fn removals_are_emitted_before_additions() {
        let mut tracker: ChangeTracker<TestFile> = ChangeTracker::default();
        let a = file("a");
        // First round: "a" is added.
        tracker.get_changes::<()>(vec![a.clone()], vec![]);

        // Second round: "a" is gone, "b" appears. The removal of "a" must be
        // emitted before the addition of "b".
        let b = file("b");
        let changes = tracker.get_changes::<()>(vec![b.clone()], vec![]);

        assert_eq!(changes.len(), 2);
        match &changes[0] {
            Change::Remove(f) => assert_eq!(f.path().name, "a"),
            _ => panic!("expected the removal to be emitted first"),
        }
        match &changes[1] {
            Change::Add(f) => assert_eq!(f.path().name, "b"),
            _ => panic!("expected the addition to be emitted after the removal"),
        }
    }
}
