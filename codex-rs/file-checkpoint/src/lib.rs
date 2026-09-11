//! Durable, turn-scoped before-images for workspace files changed by managed tools.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;

use codex_file_system::ExecutorFileSystem;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::LegacyAppPathString;
use codex_utils_path_uri::PathUri;
use serde::Serialize;
use thiserror::Error;
use tokio::sync::Semaphore;

mod api;
mod model;
mod storage;

pub type FileSystemsByEnvironment = HashMap<String, Arc<dyn ExecutorFileSystem>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileImage {
    Absent,
    Contents(Vec<u8>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileAfterImage {
    pub path: PathUri,
    pub image: FileImage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FileRestoreChangeKind {
    Create,
    Update,
    Delete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FileRestoreDisposition {
    Restorable,
    Conflict,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRestorePreviewEntry {
    pub environment_id: String,
    pub path: LegacyAppPathString,
    pub change_kind: FileRestoreChangeKind,
    pub disposition: FileRestoreDisposition,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRestorePreview {
    pub files: Vec<FileRestorePreviewEntry>,
}

impl FileRestorePreview {
    pub fn restorable_file_count(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.disposition == FileRestoreDisposition::Restorable)
            .count()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRestoreOutcome {
    pub restored: Vec<FileRestorePreviewEntry>,
    pub skipped: Vec<FileRestorePreviewEntry>,
    pub failed: Vec<FileRestorePreviewEntry>,
}

#[derive(Debug, Error)]
pub enum FileCheckpointError {
    #[error("file checkpoint I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("file checkpoint data is invalid: {0}")]
    InvalidData(String),
    #[error("file checkpoint state lock was poisoned")]
    LockPoisoned,
    #[error("file checkpoint operation lock was closed")]
    OperationLockClosed,
}

/// Durable checkpoint history for one Codex thread.
pub struct FileCheckpointStore {
    thread_dir: AbsolutePathBuf,
    journal_path: AbsolutePathBuf,
    blobs_dir: AbsolutePathBuf,
    state: Mutex<model::CheckpointState>,
    operation_lock: Semaphore,
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
