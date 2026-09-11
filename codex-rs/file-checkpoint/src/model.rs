use std::collections::HashMap;

use codex_utils_path_uri::LegacyAppPathString;
use codex_utils_path_uri::PathUri;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

use crate::FileCheckpointError;
use crate::FileImage;
use crate::FileRestoreChangeKind;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileKey {
    pub(super) environment_id: String,
    pub(super) path: LegacyAppPathString,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum BeforeImage {
    Absent,
    Blob { sha256: String, size: u64 },
}

impl BeforeImage {
    pub(super) fn identity(&self) -> ImageIdentity {
        match self {
            Self::Absent => ImageIdentity::Absent,
            Self::Blob { sha256, size } => ImageIdentity::Present {
                sha256: sha256.clone(),
                size: *size,
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ImageIdentity {
    Absent,
    Present { sha256: String, size: u64 },
}

#[derive(Clone, Debug, Default)]
pub(super) struct TurnCheckpoint {
    pub(super) turn_id: String,
    pub(super) before_by_file: HashMap<FileKey, BeforeImage>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct CheckpointState {
    pub(super) turns: Vec<TurnCheckpoint>,
    pub(super) expected_by_file: HashMap<FileKey, ImageIdentity>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum JournalEntry {
    BeginTurn {
        turn_id: String,
    },
    BeforeImage {
        turn_id: String,
        file: FileKey,
        before: BeforeImage,
    },
    AfterImage {
        file: FileKey,
        after: ImageIdentity,
    },
    Restore {
        restored: Vec<RestoredIdentity>,
    },
    DiscardFromTurn {
        turn_id: String,
    },
    PruneTurn {
        turn_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct RestoredIdentity {
    pub(super) file: FileKey,
    pub(super) image: ImageIdentity,
}

impl CheckpointState {
    pub(super) fn apply(&mut self, entry: JournalEntry) {
        match entry {
            JournalEntry::BeginTurn { turn_id } => {
                if !self.turns.iter().any(|turn| turn.turn_id == turn_id) {
                    self.turns.push(TurnCheckpoint {
                        turn_id,
                        before_by_file: HashMap::new(),
                    });
                }
            }
            JournalEntry::BeforeImage {
                turn_id,
                file,
                before,
            } => {
                if let Some(turn) = self.turns.iter_mut().find(|turn| turn.turn_id == turn_id) {
                    turn.before_by_file
                        .entry(file.clone())
                        .or_insert_with(|| before.clone());
                    self.expected_by_file.insert(file, before.identity());
                }
            }
            JournalEntry::AfterImage { file, after } => {
                self.expected_by_file.insert(file, after);
            }
            JournalEntry::Restore { restored } => {
                self.expected_by_file.extend(
                    restored
                        .into_iter()
                        .map(|restored| (restored.file, restored.image)),
                );
            }
            JournalEntry::DiscardFromTurn { turn_id } => {
                if let Some(start) = self.turns.iter().position(|turn| turn.turn_id == turn_id) {
                    self.turns.truncate(start);
                    self.retain_tracked_files();
                }
            }
            JournalEntry::PruneTurn { turn_id } => {
                self.turns.retain(|turn| turn.turn_id != turn_id);
                self.retain_tracked_files();
            }
        }
    }

    pub(super) fn restore_plan(&self, before_turn_id: &str) -> Vec<PlannedRestore> {
        let Some(start) = self
            .turns
            .iter()
            .position(|turn| turn.turn_id == before_turn_id)
        else {
            return Vec::new();
        };
        let mut target_by_file = HashMap::new();
        for turn in &self.turns[start..] {
            for (file, before) in &turn.before_by_file {
                target_by_file
                    .entry(file.clone())
                    .or_insert_with(|| before.clone());
            }
        }
        let mut plan = target_by_file
            .into_iter()
            .map(|(file, target)| PlannedRestore {
                expected_current: self.expected_by_file.get(&file).cloned(),
                file,
                target,
            })
            .collect::<Vec<_>>();
        plan.sort_by(|left, right| {
            left.file
                .environment_id
                .cmp(&right.file.environment_id)
                .then_with(|| left.file.path.as_str().cmp(right.file.path.as_str()))
        });
        plan
    }

    fn retain_tracked_files(&mut self) {
        let tracked = self
            .turns
            .iter()
            .flat_map(|turn| turn.before_by_file.keys())
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        self.expected_by_file
            .retain(|file, _| tracked.contains(file));
    }
}

#[derive(Clone, Debug)]
pub(super) struct PlannedRestore {
    pub(super) file: FileKey,
    pub(super) target: BeforeImage,
    pub(super) expected_current: Option<ImageIdentity>,
}

pub(super) fn persisted_path(path: &PathUri) -> Result<LegacyAppPathString, FileCheckpointError> {
    let convention = path.infer_path_convention().ok_or_else(|| {
        FileCheckpointError::InvalidData(format!("path convention is unknown for {path}"))
    })?;
    LegacyAppPathString::from_path_uri(path, convention)
        .map_err(|err| FileCheckpointError::InvalidData(err.to_string()))
}

pub(super) fn identity_from_image(image: &FileImage) -> ImageIdentity {
    match image {
        FileImage::Absent => ImageIdentity::Absent,
        FileImage::Contents(contents) => ImageIdentity::Present {
            sha256: sha256(contents),
            size: u64::try_from(contents.len()).unwrap_or(u64::MAX),
        },
    }
}

pub(super) fn change_kind(current: &ImageIdentity, target: &BeforeImage) -> FileRestoreChangeKind {
    match (current, target) {
        (ImageIdentity::Absent, BeforeImage::Blob { .. }) => FileRestoreChangeKind::Create,
        (ImageIdentity::Present { .. }, BeforeImage::Absent) => FileRestoreChangeKind::Delete,
        (ImageIdentity::Present { .. }, BeforeImage::Blob { .. }) => FileRestoreChangeKind::Update,
        (ImageIdentity::Absent, BeforeImage::Absent) => FileRestoreChangeKind::Update,
    }
}

pub(super) fn sha256(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

pub(super) fn validate_sha256(value: &str) -> Result<(), FileCheckpointError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(FileCheckpointError::InvalidData(
            "checkpoint blob name is not a SHA-256 digest".to_string(),
        ))
    }
}
