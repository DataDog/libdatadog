// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Kafka cluster ids, shared between the processes of a sidecar.
//!
//! Data Streams Monitoring tags Kafka checkpoints with the id of the cluster a message goes to. A
//! Kafka client learns that id from the first metadata response its brokers send, so a client
//! created for a single request, as PHP's are, does not know it yet when it produces its first
//! message. A process that has learned an id reports it with the client's bootstrap servers; the
//! sidecar keeps one map from bootstrap servers to cluster id and publishes it in a shared-memory
//! segment that all of its processes read, so a new process can tag its first produce with the id
//! another process learned.

use libdd_common::MutexExt;
use libdd_ipc::one_way_shared_memory::{OneWayShmReader, OneWayShmWriter, open_named_shm};
use libdd_ipc::platform::{MappedMem, NamedShmHandle};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::sync::{Arc, Mutex};
use tracing::{error, warn};

/// Bootstrap servers and cluster ids longer than this are not kept.
const MAX_VALUE_LEN: usize = 1024;

/// Bootstrap servers kept, so that the map stays small whatever clients report.
const MAX_CLUSTERS: usize = 64;

fn shm_path() -> CString {
    let mut path = format!("/dddkc{}", crate::shm_namespace());
    if cfg!(unix) {
        path.truncate(31);
    }

    #[allow(clippy::unwrap_used)]
    CString::new(path).unwrap()
}

fn is_keepable(bootstrap_servers: &str, cluster_id: &str) -> bool {
    !bootstrap_servers.is_empty()
        && !cluster_id.is_empty()
        && bootstrap_servers.len() <= MAX_VALUE_LEN
        && cluster_id.len() <= MAX_VALUE_LEN
}

/// The sidecar's side: the cluster ids its processes reported, and the segment they are published
/// in, created on the first report.
#[derive(Default, Clone)]
pub struct KafkaClusterIds(Arc<Mutex<Published>>);

#[derive(Default)]
struct Published {
    ids: BTreeMap<String, String>,
    writer: Option<OneWayShmWriter<NamedShmHandle>>,
}

impl KafkaClusterIds {
    /// Records that clients with `bootstrap_servers` reach the cluster `cluster_id`, and publishes
    /// the map if that is new.
    pub fn set(&self, bootstrap_servers: String, cluster_id: String) {
        self.set_at(shm_path, bootstrap_servers, cluster_id)
    }

    fn set_at(&self, path: fn() -> CString, bootstrap_servers: String, cluster_id: String) {
        if !is_keepable(&bootstrap_servers, &cluster_id) {
            return;
        }
        let mut published = self.0.lock_or_panic();
        if published.ids.get(&bootstrap_servers) == Some(&cluster_id) {
            return;
        }
        if !published.ids.contains_key(&bootstrap_servers) && published.ids.len() >= MAX_CLUSTERS {
            warn!(
                "Not sharing the Kafka cluster id of {bootstrap_servers}: {MAX_CLUSTERS} clusters \
                 are already shared"
            );
            return;
        }
        published.ids.insert(bootstrap_servers, cluster_id);

        if published.writer.is_none() {
            published.writer = match OneWayShmWriter::<NamedShmHandle>::new(path()) {
                Ok(writer) => Some(writer),
                Err(e) => {
                    error!("Failed creating the shared memory for Kafka cluster ids: {e:?}");
                    None
                }
            };
        }
        if let Some(writer) = &published.writer {
            match bincode::serialize(&published.ids) {
                // A payload that does not fit is logged and dropped by the writer; readers keep
                // the previous map.
                Ok(buf) => _ = writer.write(&buf),
                Err(e) => error!("Failed serializing the Kafka cluster ids: {e:?}"),
            }
        }
    }
}

/// A process's side: the map the sidecar published, and the ids the process learned itself and
/// the sidecar has not published yet.
pub struct KafkaClusterIdReader {
    /// `extra` names the segment to read in tests; `None` is the sidecar's segment.
    reader: OneWayShmReader<NamedShmHandle, Option<CString>>,
    ids: BTreeMap<String, String>,
}

fn open(path: &Option<CString>) -> Option<MappedMem<NamedShmHandle>> {
    match path {
        Some(path) => open_named_shm(path).ok(),
        None => open_named_shm(&shm_path()).ok(),
    }
}

impl Default for KafkaClusterIdReader {
    fn default() -> Self {
        Self::at(None)
    }
}

impl KafkaClusterIdReader {
    fn at(path: Option<CString>) -> Self {
        KafkaClusterIdReader {
            reader: OneWayShmReader::new_with_opener(open(&path), path, open),
            ids: BTreeMap::new(),
        }
    }

    /// Moves to the segment of the sidecar the process now talks to, if it changed.
    pub fn reconnect(&self) {
        match &self.reader.extra {
            Some(path) => self.reader.reconnect(path),
            None => self.reader.reconnect(&shm_path()),
        }
    }

    /// The cluster id of the clients with `bootstrap_servers`, if a process learned it.
    pub fn get(&mut self, bootstrap_servers: &str) -> Option<&str> {
        self.refresh();
        self.ids.get(bootstrap_servers).map(String::as_str)
    }

    /// Remembers that clients with `bootstrap_servers` reach `cluster_id`, as this process read it
    /// from one of its clients. Returns whether that is news to the sidecar, so that the caller
    /// reports it; the process uses it until the sidecar publishes its next map.
    pub fn learn(&mut self, bootstrap_servers: &str, cluster_id: &str) -> bool {
        if !is_keepable(bootstrap_servers, cluster_id) {
            return false;
        }
        self.refresh();
        if self.ids.get(bootstrap_servers).map(String::as_str) == Some(cluster_id) {
            return false;
        }
        if self.ids.len() < MAX_CLUSTERS || self.ids.contains_key(bootstrap_servers) {
            self.ids
                .insert(bootstrap_servers.to_owned(), cluster_id.to_owned());
        }
        true
    }

    fn refresh(&mut self) {
        let (changed, data) = self.reader.read();
        if changed && !data.is_empty() {
            match bincode::deserialize(data) {
                Ok(ids) => self.ids = ids,
                Err(e) => error!("Failed deserializing the Kafka cluster ids: {e:?}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT_PATH: AtomicU32 = AtomicU32::new(0);

    thread_local! {
        static PATH: CString = CString::new(format!(
            "/dddkct{}-{}",
            std::process::id(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        ))
        .unwrap();
    }

    fn test_path() -> CString {
        PATH.with(Clone::clone)
    }

    fn reader() -> KafkaClusterIdReader {
        KafkaClusterIdReader::at(Some(test_path()))
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_reader_sees_an_id_once_it_is_published() {
        let ids = KafkaClusterIds::default();
        let mut reader = reader();
        assert_eq!(reader.get("kafka:9092"), None);

        ids.set_at(
            test_path,
            "kafka:9092".into(),
            "5L6g3nShT-eMCtK--X86sw".into(),
        );
        assert_eq!(reader.get("kafka:9092"), Some("5L6g3nShT-eMCtK--X86sw"));
        assert_eq!(reader.get("other:9092"), None);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_new_id_replaces_the_old_one_and_a_known_one_is_not_published_again() {
        let ids = KafkaClusterIds::default();
        let mut reader = reader();
        ids.set_at(test_path, "kafka:9092".into(), "c1".into());
        assert_eq!(reader.get("kafka:9092"), Some("c1"));

        ids.set_at(test_path, "kafka:9092".into(), "c1".into());
        assert!(!reader.reader.read().0, "a known id is not published again");

        ids.set_at(test_path, "kafka:9092".into(), "c2".into());
        assert_eq!(reader.get("kafka:9092"), Some("c2"));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_learned_id_is_used_and_reported_once() {
        let mut reader = reader();
        assert!(reader.learn("kafka:9092", "c1"), "news to the sidecar");
        assert_eq!(reader.get("kafka:9092"), Some("c1"));
        assert!(!reader.learn("kafka:9092", "c1"), "already reported");

        // Once the sidecar publishes it, the process reads it from there.
        KafkaClusterIds::default().set_at(test_path, "kafka:9092".into(), "c1".into());
        assert!(!reader.learn("kafka:9092", "c1"));
        assert_eq!(reader.get("kafka:9092"), Some("c1"));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn empty_and_oversized_values_are_ignored() {
        let ids = KafkaClusterIds::default();
        let long = "k".repeat(MAX_VALUE_LEN + 1);
        ids.set_at(test_path, "".into(), "c1".into());
        ids.set_at(test_path, "kafka:9092".into(), "".into());
        ids.set_at(test_path, long.clone(), "c1".into());
        let mut reader = reader();
        assert!(!reader.reader.read().0, "nothing was published");
        assert!(!reader.learn(&long, "c1"));
        assert_eq!(reader.get(&long), None);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn clusters_beyond_the_limit_are_not_kept() {
        let ids = KafkaClusterIds::default();
        for i in 0..MAX_CLUSTERS {
            ids.set_at(test_path, format!("kafka-{i}:9092"), format!("c{i}"));
        }
        ids.set_at(test_path, "one-more:9092".into(), "c".into());
        let mut reader = reader();
        assert_eq!(reader.get("kafka-0:9092"), Some("c0"));
        assert_eq!(reader.get("one-more:9092"), None);

        // A known cluster still gets its new id.
        ids.set_at(test_path, "kafka-0:9092".into(), "new".into());
        assert_eq!(reader.get("kafka-0:9092"), Some("new"));
    }
}
