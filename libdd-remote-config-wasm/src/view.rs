// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use libdd_remote_config::fetch::{
    AssignedFile, Assignment, AssignmentBatch, AssignmentChange, ScopedAssignment, WatchReason,
    WatchUpdate,
};
use serde::Serialize;
use std::sync::Arc;

#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Snapshot,
    Upsert,
    Remove,
}

pub struct Record {
    pub kind: Kind,
    pub file: Arc<AssignedFile>,
    pub previous: Option<Arc<AssignedFile>>,
}

pub struct View {
    pub owner: i32,
    pub watch: Option<WatchTicket>,
    desired: Desired,
    records: Vec<Record>,
}

pub struct WatchTicket {
    pub watch: i32,
    pub ticket: i32,
    reason: WatchReason,
}

#[derive(Clone)]
enum Desired {
    Session(Arc<Assignment>),
    Watch(ScopedAssignment),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileDescriptor<'file> {
    path: &'file str,
    product: &'static str,
    config_id: &'file str,
    name: &'file str,
    version: String,
    hashes: Vec<(&'file str, &'file str)>,
}

impl<'file> From<&'file AssignedFile> for FileDescriptor<'file> {
    fn from(file: &'file AssignedFile) -> Self {
        let path = file.path();
        Self {
            path: path.as_str(),
            product: path.product().into(),
            config_id: path.config_id(),
            name: path.name(),
            version: file.version().to_string(),
            hashes: file
                .hashes()
                .iter()
                .map(|hash| (hash.algorithm.as_str(), hash.hash.as_str()))
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct RecordDescriptor<'file> {
    kind: Kind,
    file: FileDescriptor<'file>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous: Option<FileDescriptor<'file>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ViewDescriptor<'file> {
    generation: String,
    target_generation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    receipt_owner: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<WatchReason>,
    records: Vec<RecordDescriptor<'file>>,
}

impl View {
    pub fn snapshot(owner: i32, desired: Arc<Assignment>) -> Self {
        Self::from_snapshot(owner, Desired::Session(desired))
    }

    fn from_snapshot(owner: i32, desired: Desired) -> Self {
        let record = |file: &Arc<AssignedFile>| Record {
            kind: Kind::Snapshot,
            file: file.clone(),
            previous: None,
        };
        let records = match &desired {
            Desired::Session(assignment) => assignment.files().map(record).collect(),
            Desired::Watch(assignment) => assignment.files().map(record).collect(),
        };
        Self {
            owner,
            watch: None,
            desired,
            records,
        }
    }

    pub fn desired_view(&self) -> Self {
        Self::from_snapshot(self.owner, self.desired.clone())
    }

    pub fn grants_receipts(&self) -> bool {
        matches!(self.desired, Desired::Session(_))
    }

    pub fn batch(owner: i32, batch: &AssignmentBatch) -> Self {
        Self {
            owner,
            watch: None,
            desired: Desired::Session(batch.desired.clone()),
            records: Self::change_records(&batch.changes),
        }
    }

    pub fn watch(owner: i32, watch: i32, update: WatchUpdate) -> Self {
        Self {
            owner,
            watch: Some(WatchTicket {
                watch,
                ticket: update.ticket,
                reason: update.reason,
            }),
            records: Self::change_records(update.changes()),
            desired: Desired::Watch(update.desired),
        }
    }

    fn change_records(changes: &[AssignmentChange]) -> Vec<Record> {
        changes
            .iter()
            .map(|change| match change {
                AssignmentChange::Upsert { file, previous } => Record {
                    kind: Kind::Upsert,
                    file: file.clone(),
                    previous: previous.clone(),
                },
                AssignmentChange::Remove(file) => Record {
                    kind: Kind::Remove,
                    file: file.clone(),
                    previous: None,
                },
            })
            .collect()
    }

    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    pub fn record(&self, index: i32) -> Result<&Record> {
        self.records
            .get(usize::try_from(index)?)
            .context("invalid view index")
    }

    pub fn describe(&self) -> impl Serialize + '_ {
        let (generation, target_generation) = match &self.desired {
            Desired::Session(assignment) => (assignment.generation, assignment.target_generation),
            Desired::Watch(assignment) => (assignment.generation(), assignment.target_generation()),
        };
        ViewDescriptor {
            generation: generation.to_string(),
            target_generation: target_generation.to_string(),
            receipt_owner: self.grants_receipts().then_some(self.owner),
            reason: self.watch.as_ref().map(|watch| watch.reason),
            records: self
                .records
                .iter()
                .map(|record| RecordDescriptor {
                    kind: record.kind,
                    file: FileDescriptor::from(record.file.as_ref()),
                    previous: record.previous.as_deref().map(FileDescriptor::from),
                })
                .collect(),
        }
    }
}
