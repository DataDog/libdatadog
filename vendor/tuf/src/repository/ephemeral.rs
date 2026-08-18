//! Repository implementation backed by memory

use futures_io::AsyncRead;
use futures_util::future::{BoxFuture, FutureExt};
use futures_util::io::{AsyncReadExt, Cursor};
use std::collections::HashMap;
use std::marker::PhantomData;

use crate::error::Error;
use crate::interchange::DataInterchange;
use crate::metadata::{MetadataPath, MetadataVersion, TargetPath};
use crate::repository::{RepositoryProvider, RepositoryStorage};
use crate::Result;

/// An ephemeral repository contained solely in memory.
#[derive(Debug, Default)]
pub struct EphemeralRepository<D> {
    metadata: HashMap<(MetadataPath, MetadataVersion), Box<[u8]>>,
    targets: HashMap<TargetPath, Box<[u8]>>,
    _interchange: PhantomData<D>,
}

impl<D> EphemeralRepository<D>
where
    D: DataInterchange,
{
    /// Create a new ephemeral repository.
    pub fn new() -> Self {
        Self {
            metadata: HashMap::new(),
            targets: HashMap::new(),
            _interchange: PhantomData,
        }
    }

    /// Returns a [EphemeralBatchUpdate] for manipulating this repository. This allows callers to
    /// stage a number of mutations, and optionally atomically write them all at once.
    pub fn batch_update(&mut self) -> EphemeralBatchUpdate<'_, D> {
        EphemeralBatchUpdate {
            parent_repo: self,
            staging_repo: EphemeralRepository::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn metadata(&self) -> &HashMap<(MetadataPath, MetadataVersion), Box<[u8]>> {
        &self.metadata
    }

    /// Synchronously fetch the raw bytes of a stored target, without going
    /// through the async [`RepositoryProvider`] trait.
    ///
    /// Since an [`EphemeralRepository`] is fully in-memory, no I/O is required
    /// to read the target and no async runtime is needed. Returns
    /// [`Error::TargetNotFound`] if the target has not been stored.
    pub fn get_target(&self, target_path: &TargetPath) -> Result<&[u8]> {
        self.targets
            .get(target_path)
            .map(|b| &**b)
            .ok_or_else(|| Error::TargetNotFound(target_path.clone()))
    }
}

impl<D> RepositoryProvider<D> for EphemeralRepository<D>
where
    D: DataInterchange + Sync,
{
    fn fetch_metadata<'a>(
        &'a self,
        meta_path: &MetadataPath,
        version: MetadataVersion,
    ) -> BoxFuture<'a, Result<Box<dyn AsyncRead + Send + Unpin + 'a>>> {
        let bytes = match self.metadata.get(&(meta_path.clone(), version)) {
            Some(bytes) => Ok(bytes),
            None => Err(Error::MetadataNotFound {
                path: meta_path.clone(),
                version,
            }),
        };
        bytes_to_reader(bytes).boxed()
    }

    fn fetch_target<'a>(
        &'a self,
        target_path: &TargetPath,
    ) -> BoxFuture<'a, Result<Box<dyn AsyncRead + Send + Unpin + 'a>>> {
        let bytes = match self.targets.get(target_path) {
            Some(bytes) => Ok(bytes),
            None => Err(Error::TargetNotFound(target_path.clone())),
        };
        bytes_to_reader(bytes).boxed()
    }
}

impl<D> RepositoryStorage<D> for EphemeralRepository<D>
where
    D: DataInterchange + Sync,
{
    fn store_metadata<'a>(
        &'a mut self,
        meta_path: &MetadataPath,
        version: MetadataVersion,
        metadata: &'a mut (dyn AsyncRead + Send + Unpin + 'a),
    ) -> BoxFuture<'a, Result<()>> {
        let meta_path = meta_path.clone();
        let self_metadata = &mut self.metadata;
        async move {
            let mut buf = Vec::new();
            metadata.read_to_end(&mut buf).await?;
            buf.shrink_to_fit();
            self_metadata.insert((meta_path, version), buf.into_boxed_slice());
            Ok(())
        }
        .boxed()
    }

    fn store_target<'a>(
        &'a mut self,
        target_path: &TargetPath,
        read: &'a mut (dyn AsyncRead + Send + Unpin + 'a),
    ) -> BoxFuture<'a, Result<()>> {
        let target_path = target_path.clone();
        let self_targets = &mut self.targets;
        async move {
            let mut buf = Vec::new();
            read.read_to_end(&mut buf).await?;
            buf.shrink_to_fit();
            self_targets.insert(target_path.clone(), buf.into_boxed_slice());
            Ok(())
        }
        .boxed()
    }
}

/// [EphemeralBatchUpdate] is a special repository that is designed to write the metadata and
/// targets to an [EphemeralRepository] in a single batch.
///
/// Note: `EphemeralBatchUpdate::commit()` must be called in order to write the metadata and
/// targets to the [EphemeralRepository]. Otherwise any queued changes will be lost on drop.
#[derive(Debug)]
pub struct EphemeralBatchUpdate<'a, D> {
    parent_repo: &'a mut EphemeralRepository<D>,
    staging_repo: EphemeralRepository<D>,
}

impl<'a, D> EphemeralBatchUpdate<'a, D>
where
    D: DataInterchange + Sync,
{
    /// Synchronously fetch the raw bytes of a stored target, without going
    /// through the async [`RepositoryProvider`] trait.
    ///
    /// The staged (uncommitted) targets shadow the parent's targets, matching
    /// the behavior of [`RepositoryProvider::fetch_target`].
    pub fn get_target(&self, target_path: &TargetPath) -> Result<&[u8]> {
        if let Some(bytes) = self.staging_repo.targets.get(target_path) {
            Ok(&**bytes)
        } else {
            self.parent_repo.get_target(target_path)
        }
    }

    /// Write all the metadata and targets in the [EphemeralBatchUpdate] to the source
    /// [EphemeralRepository] in a single batch operation.
    pub fn commit(self) {
        self.parent_repo
            .metadata
            .extend(self.staging_repo.metadata.into_iter());

        self.parent_repo
            .targets
            .extend(self.staging_repo.targets.into_iter());
    }
}

impl<D> RepositoryProvider<D> for EphemeralBatchUpdate<'_, D>
where
    D: DataInterchange + Sync,
{
    fn fetch_metadata<'a>(
        &'a self,
        meta_path: &MetadataPath,
        version: MetadataVersion,
    ) -> BoxFuture<'a, Result<Box<dyn AsyncRead + Send + Unpin + 'a>>> {
        let key = (meta_path.clone(), version);
        let bytes = if let Some(bytes) = self.staging_repo.metadata.get(&key) {
            Ok(bytes)
        } else {
            self.parent_repo
                .metadata
                .get(&key)
                .ok_or_else(|| Error::MetadataNotFound {
                    path: meta_path.clone(),
                    version,
                })
        };
        bytes_to_reader(bytes).boxed()
    }

    fn fetch_target<'a>(
        &'a self,
        target_path: &TargetPath,
    ) -> BoxFuture<'a, Result<Box<dyn AsyncRead + Send + Unpin + 'a>>> {
        let bytes = if let Some(bytes) = self.staging_repo.targets.get(target_path) {
            Ok(bytes)
        } else {
            self.parent_repo
                .targets
                .get(target_path)
                .ok_or_else(|| Error::TargetNotFound(target_path.clone()))
        };
        bytes_to_reader(bytes).boxed()
    }
}

impl<D> RepositoryStorage<D> for EphemeralBatchUpdate<'_, D>
where
    D: DataInterchange + Sync,
{
    fn store_metadata<'a>(
        &'a mut self,
        meta_path: &MetadataPath,
        version: MetadataVersion,
        metadata: &'a mut (dyn AsyncRead + Send + Unpin + 'a),
    ) -> BoxFuture<'a, Result<()>> {
        self.staging_repo
            .store_metadata(meta_path, version, metadata)
    }

    fn store_target<'a>(
        &'a mut self,
        target_path: &TargetPath,
        read: &'a mut (dyn AsyncRead + Send + Unpin + 'a),
    ) -> BoxFuture<'a, Result<()>> {
        self.staging_repo.store_target(target_path, read)
    }
}

#[allow(clippy::borrowed_box)]
async fn bytes_to_reader(
    bytes: Result<&'_ Box<[u8]>>,
) -> Result<Box<dyn AsyncRead + Send + Unpin + '_>> {
    let bytes = bytes?;
    let reader: Box<dyn AsyncRead + Send + Unpin> = Box::new(Cursor::new(bytes));
    Ok(reader)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::interchange::Json;
    use crate::repository::{fetch_metadata_to_string, fetch_target_to_string};
    use assert_matches::assert_matches;
    use futures_executor::block_on;

    #[test]
    fn ephemeral_repo_targets() {
        block_on(async {
            let mut repo = EphemeralRepository::<Json>::new();

            let path = TargetPath::new("batty").unwrap();
            if let Err(err) = repo.fetch_target(&path).await {
                assert_matches!(err, Error::TargetNotFound(p) if p == path);
            } else {
                panic!("expected fetch_target to fail");
            }

            let data: &[u8] = b"like tears in the rain";
            let path = TargetPath::new("batty").unwrap();
            repo.store_target(&path, &mut &*data).await.unwrap();

            let mut read = repo.fetch_target(&path).await.unwrap();
            let mut buf = Vec::new();
            read.read_to_end(&mut buf).await.unwrap();
            assert_eq!(buf.as_slice(), data);
            drop(read);

            // RepositoryProvider implementations do not guarantee data is not corrupt.
            let bad_data: &[u8] = b"you're in a desert";
            repo.store_target(&path, &mut &*bad_data).await.unwrap();
            let mut read = repo.fetch_target(&path).await.unwrap();
            buf.clear();
            read.read_to_end(&mut buf).await.unwrap();
            assert_eq!(buf.as_slice(), bad_data);
        })
    }

    #[test]
    fn ephemeral_repo_get_target_sync() {
        block_on(async {
            let mut repo = EphemeralRepository::<Json>::new();
            let path = TargetPath::new("sync-target").unwrap();

            // Missing target returns TargetNotFound.
            assert_matches!(
                repo.get_target(&path),
                Err(Error::TargetNotFound(p)) if p == path
            );

            // After storing, we can read the bytes back synchronously.
            let data: &[u8] = b"in-memory bytes";
            repo.store_target(&path, &mut &*data).await.unwrap();
            assert_eq!(repo.get_target(&path).unwrap(), data);

            // Batch update: staged bytes shadow the parent's bytes, and the
            // parent's bytes remain visible for un-staged targets.
            let other_path = TargetPath::new("other").unwrap();
            let other_data: &[u8] = b"parent only";
            repo.store_target(&other_path, &mut &*other_data)
                .await
                .unwrap();

            let mut batch = repo.batch_update();
            let staged: &[u8] = b"staged bytes";
            batch.store_target(&path, &mut &*staged).await.unwrap();
            assert_eq!(batch.get_target(&path).unwrap(), staged);
            assert_eq!(batch.get_target(&other_path).unwrap(), other_data);

            // Dropping the batch without committing leaves the parent unchanged.
            drop(batch);
            assert_eq!(repo.get_target(&path).unwrap(), data);
        })
    }

    #[test]
    fn ephemeral_repo_batch_update() {
        block_on(async {
            let mut repo = EphemeralRepository::<Json>::new();

            let meta_path = MetadataPath::new("meta").unwrap();
            let meta_version = MetadataVersion::None;
            let target_path = TargetPath::new("target").unwrap();

            // First, write some stuff to the repository.
            let committed_meta = "committed meta";
            let committed_target = "committed target";

            repo.store_metadata(&meta_path, meta_version, &mut committed_meta.as_bytes())
                .await
                .unwrap();

            repo.store_target(&target_path, &mut committed_target.as_bytes())
                .await
                .unwrap();

            let mut batch = repo.batch_update();

            // Make sure we can read back the committed stuff.
            assert_eq!(
                fetch_metadata_to_string(&batch, &meta_path, meta_version)
                    .await
                    .unwrap(),
                committed_meta,
            );
            assert_eq!(
                fetch_target_to_string(&batch, &target_path).await.unwrap(),
                committed_target,
            );

            // Next, stage some stuff in the batch_update.
            let staged_meta = "staged meta";
            let staged_target = "staged target";
            batch
                .store_metadata(&meta_path, meta_version, &mut staged_meta.as_bytes())
                .await
                .unwrap();
            batch
                .store_target(&target_path, &mut staged_target.as_bytes())
                .await
                .unwrap();

            // Make sure it got staged.
            assert_eq!(
                fetch_metadata_to_string(&batch, &meta_path, meta_version)
                    .await
                    .unwrap(),
                staged_meta,
            );
            assert_eq!(
                fetch_target_to_string(&batch, &target_path).await.unwrap(),
                staged_target,
            );

            // Next, drop the batch_update. We shouldn't have written the data back to the
            // repository.
            drop(batch);

            assert_eq!(
                fetch_metadata_to_string(&repo, &meta_path, meta_version)
                    .await
                    .unwrap(),
                committed_meta,
            );
            assert_eq!(
                fetch_target_to_string(&repo, &target_path).await.unwrap(),
                committed_target,
            );

            // Do the batch_update again, but this time write the data.
            let mut batch = repo.batch_update();
            batch
                .store_metadata(&meta_path, meta_version, &mut staged_meta.as_bytes())
                .await
                .unwrap();
            batch
                .store_target(&target_path, &mut staged_target.as_bytes())
                .await
                .unwrap();
            batch.commit();

            // Make sure the new data got to the repository.
            assert_eq!(
                fetch_metadata_to_string(&repo, &meta_path, meta_version)
                    .await
                    .unwrap(),
                staged_meta,
            );
            assert_eq!(
                fetch_target_to_string(&repo, &target_path).await.unwrap(),
                staged_target,
            );
        })
    }
}
